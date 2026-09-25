//! Who made this machine, guessed from the first three octets of its MAC.
//!
//! The first thing a network boot server learns about a machine is its
//! hardware address, and the first three octets of that are an IEEE
//! assignment. It is not identity — a NIC can be swapped and a hypervisor
//! invents its own — but it is enough to say "this is a Dell" or "this is a
//! virtual machine", which is exactly the granularity at which somebody wants
//! to say "and it gets the Dell image".
//!
//! The built-in table covers the assignments a lab actually meets. The full
//! IEEE registry is 35,000 rows and has no business being compiled in, so
//! `OuiDatabase::load` reads it from disk when an operator wants all of it.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::mac::MacAddr;

/// What kind of thing this is, as far as can be told from the address.
///
/// Coarse on purpose. "Dell" is a vendor and "a virtual machine" is a class,
/// and a rule wants to say both — `vendor = ["Dell"]` for the driver bundle,
/// `device_class = ["virtual"]` for "never wipe the disk on these".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceClass {
    /// Real hardware with a real NIC.
    Physical,
    /// A hypervisor's synthetic NIC: VMware, KVM, Hyper-V, Xen, VirtualBox.
    Virtual,
    /// A single-board computer — a Pi, a Jetson, an Odroid.
    Sbc,
    /// Switching and routing gear that happens to PXE boot.
    Network,
    /// The address is locally administered or unassigned: nothing to go on.
    Unknown,
}

impl DeviceClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceClass::Physical => "physical",
            DeviceClass::Virtual => "virtual",
            DeviceClass::Sbc => "sbc",
            DeviceClass::Network => "network",
            DeviceClass::Unknown => "unknown",
        }
    }
}

/// What the address told us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identification {
    /// The assignee's name, if the prefix is in the table.
    pub vendor: Option<String>,
    pub device_class: DeviceClass,
    /// The prefix that produced this, as `aa:bb:cc`.
    pub oui: String,
    /// Whether the address was minted locally rather than assigned. A `true`
    /// here is why `vendor` is so often `None`.
    pub locally_administered: bool,
}

impl Identification {
    pub fn vendor_or_unknown(&self) -> &str {
        self.vendor.as_deref().unwrap_or("unknown")
    }
}

/// One built-in row.
struct Assignment {
    prefix: [u8; 3],
    vendor: &'static str,
    class: DeviceClass,
}

const fn a(prefix: [u8; 3], vendor: &'static str, class: DeviceClass) -> Assignment {
    Assignment { prefix, vendor, class }
}

/// The prefixes a lab meets, rather than all 35,000 of them.
///
/// The hypervisor block at the top is the one that earns its keep: those six
/// prefixes are most of what a homelab boots, and "is this a VM" is the
/// question a rule asks first.
const BUILT_IN: &[Assignment] = &[
    // --- hypervisors -------------------------------------------------------
    a([0x00, 0x50, 0x56], "VMware", DeviceClass::Virtual),
    a([0x00, 0x0c, 0x29], "VMware", DeviceClass::Virtual),
    a([0x00, 0x05, 0x69], "VMware", DeviceClass::Virtual),
    a([0x00, 0x1c, 0x14], "VMware", DeviceClass::Virtual),
    a([0x08, 0x00, 0x27], "VirtualBox", DeviceClass::Virtual),
    a([0x0a, 0x00, 0x27], "VirtualBox", DeviceClass::Virtual),
    a([0x52, 0x54, 0x00], "QEMU/KVM", DeviceClass::Virtual),
    a([0x00, 0x16, 0x3e], "Xen", DeviceClass::Virtual),
    a([0x00, 0x15, 0x5d], "Microsoft Hyper-V", DeviceClass::Virtual),
    a([0x00, 0x03, 0xff], "Microsoft Hyper-V", DeviceClass::Virtual),
    a([0x00, 0x1c, 0x42], "Parallels", DeviceClass::Virtual),
    a([0x02, 0x42, 0xac], "Docker", DeviceClass::Virtual),
    a([0xfa, 0x16, 0x3e], "OpenStack", DeviceClass::Virtual),
    // --- single-board computers -------------------------------------------
    a([0xb8, 0x27, 0xeb], "Raspberry Pi", DeviceClass::Sbc),
    a([0xdc, 0xa6, 0x32], "Raspberry Pi", DeviceClass::Sbc),
    a([0xe4, 0x5f, 0x01], "Raspberry Pi", DeviceClass::Sbc),
    a([0x28, 0xcd, 0xc1], "Raspberry Pi", DeviceClass::Sbc),
    a([0xd8, 0x3a, 0xdd], "Raspberry Pi", DeviceClass::Sbc),
    a([0x2c, 0xcf, 0x67], "Raspberry Pi", DeviceClass::Sbc),
    a([0x00, 0x1e, 0x06], "Hardkernel (Odroid)", DeviceClass::Sbc),
    a([0x48, 0xb0, 0x2d], "NVIDIA (Jetson)", DeviceClass::Sbc),
    a([0x00, 0x04, 0x4b], "NVIDIA", DeviceClass::Sbc),
    // --- Dell --------------------------------------------------------------
    a([0x18, 0x66, 0xda], "Dell", DeviceClass::Physical),
    a([0xb0, 0x83, 0xfe], "Dell", DeviceClass::Physical),
    a([0x00, 0x14, 0x22], "Dell", DeviceClass::Physical),
    a([0x00, 0x1e, 0xc9], "Dell", DeviceClass::Physical),
    a([0x14, 0x18, 0x77], "Dell", DeviceClass::Physical),
    a([0x84, 0x2b, 0x2b], "Dell", DeviceClass::Physical),
    a([0xd0, 0x67, 0xe5], "Dell", DeviceClass::Physical),
    a([0xf8, 0xbc, 0x12], "Dell", DeviceClass::Physical),
    a([0x54, 0xbf, 0x64], "Dell", DeviceClass::Physical),
    a([0xa4, 0xbb, 0x6d], "Dell", DeviceClass::Physical),
    a([0x6c, 0x2b, 0x59], "Dell", DeviceClass::Physical),
    a([0x00, 0x21, 0x9b], "Dell", DeviceClass::Physical),
    a([0x00, 0x24, 0xe8], "Dell", DeviceClass::Physical),
    a([0x00, 0x26, 0xb9], "Dell", DeviceClass::Physical),
    a([0x18, 0x03, 0x73], "Dell", DeviceClass::Physical),
    a([0x34, 0x17, 0xeb], "Dell", DeviceClass::Physical),
    a([0x4c, 0xd9, 0x8f], "Dell", DeviceClass::Physical),
    a([0x74, 0x86, 0x7a], "Dell", DeviceClass::Physical),
    a([0x90, 0xb1, 0x1c], "Dell", DeviceClass::Physical),
    a([0xd4, 0xae, 0x52], "Dell", DeviceClass::Physical),
    a([0xec, 0xf4, 0xbb], "Dell", DeviceClass::Physical),
    a([0xf4, 0x8e, 0x38], "Dell", DeviceClass::Physical),
    // --- HP / HPE ----------------------------------------------------------
    a([0x00, 0x1b, 0x78], "HP", DeviceClass::Physical),
    a([0x00, 0x21, 0x5a], "HP", DeviceClass::Physical),
    a([0x00, 0x25, 0xb3], "HP", DeviceClass::Physical),
    a([0x2c, 0x41, 0x38], "HP", DeviceClass::Physical),
    a([0x38, 0x63, 0xbb], "HP", DeviceClass::Physical),
    a([0x3c, 0xd9, 0x2b], "HP", DeviceClass::Physical),
    a([0x6c, 0xc2, 0x17], "HP", DeviceClass::Physical),
    a([0x8c, 0xdc, 0xd4], "HPE", DeviceClass::Physical),
    a([0x94, 0x18, 0x82], "HP", DeviceClass::Physical),
    a([0x98, 0xf2, 0xb3], "HPE", DeviceClass::Physical),
    a([0xa0, 0xd3, 0xc1], "HPE", DeviceClass::Physical),
    a([0xb4, 0xb5, 0x2f], "HP", DeviceClass::Physical),
    a([0xc4, 0x34, 0x6b], "HP", DeviceClass::Physical),
    a([0xd0, 0xbf, 0x9c], "HP", DeviceClass::Physical),
    a([0xec, 0xb1, 0xd7], "HP", DeviceClass::Physical),
    a([0xf0, 0x92, 0x1c], "HP", DeviceClass::Physical),
    // --- Lenovo ------------------------------------------------------------
    a([0x00, 0x59, 0x07], "Lenovo", DeviceClass::Physical),
    a([0x28, 0xd2, 0x44], "Lenovo", DeviceClass::Physical),
    a([0x3c, 0xf0, 0x11], "Lenovo", DeviceClass::Physical),
    a([0x54, 0xee, 0x75], "Lenovo", DeviceClass::Physical),
    a([0x6c, 0x5f, 0x1c], "Lenovo", DeviceClass::Physical),
    a([0x8c, 0x16, 0x45], "Lenovo", DeviceClass::Physical),
    a([0xe8, 0x6a, 0x64], "Lenovo", DeviceClass::Physical),
    a([0xf8, 0xb1, 0x56], "Lenovo", DeviceClass::Physical),
    a([0x00, 0x21, 0xcc], "Lenovo", DeviceClass::Physical),
    a([0x50, 0x7b, 0x9d], "Lenovo", DeviceClass::Physical),
    // --- Supermicro --------------------------------------------------------
    a([0x00, 0x25, 0x90], "Supermicro", DeviceClass::Physical),
    a([0x0c, 0xc4, 0x7a], "Supermicro", DeviceClass::Physical),
    a([0x3c, 0xec, 0xef], "Supermicro", DeviceClass::Physical),
    a([0xac, 0x1f, 0x6b], "Supermicro", DeviceClass::Physical),
    a([0x7c, 0xc2, 0x55], "Supermicro", DeviceClass::Physical),
    // --- Intel -------------------------------------------------------------
    a([0x00, 0x1b, 0x21], "Intel", DeviceClass::Physical),
    a([0x00, 0x15, 0x17], "Intel", DeviceClass::Physical),
    a([0x68, 0x05, 0xca], "Intel", DeviceClass::Physical),
    a([0x3c, 0xfd, 0xfe], "Intel", DeviceClass::Physical),
    a([0xa4, 0xbf, 0x01], "Intel", DeviceClass::Physical),
    a([0xb4, 0x96, 0x91], "Intel", DeviceClass::Physical),
    a([0x00, 0xa0, 0xc9], "Intel", DeviceClass::Physical),
    a([0x90, 0xe2, 0xba], "Intel", DeviceClass::Physical),
    a([0x8c, 0xc8, 0xf4], "Intel", DeviceClass::Physical),
    // --- other builders ----------------------------------------------------
    a([0x00, 0x19, 0x99], "Fujitsu", DeviceClass::Physical),
    a([0x90, 0x1b, 0x0e], "Fujitsu", DeviceClass::Physical),
    a([0x00, 0x1b, 0xfc], "ASUS", DeviceClass::Physical),
    a([0x2c, 0x56, 0xdc], "ASUS", DeviceClass::Physical),
    a([0x50, 0x46, 0x5d], "ASUS", DeviceClass::Physical),
    a([0x04, 0xd4, 0xc4], "ASUS", DeviceClass::Physical),
    a([0x1c, 0x1b, 0x0d], "Gigabyte", DeviceClass::Physical),
    a([0x50, 0xe5, 0x49], "Gigabyte", DeviceClass::Physical),
    a([0x94, 0xde, 0x80], "Gigabyte", DeviceClass::Physical),
    a([0xb4, 0x2e, 0x99], "Gigabyte", DeviceClass::Physical),
    a([0x00, 0x21, 0x85], "MSI", DeviceClass::Physical),
    a([0x30, 0x9c, 0x23], "MSI", DeviceClass::Physical),
    a([0xd8, 0xcb, 0x8a], "MSI", DeviceClass::Physical),
    a([0x00, 0x03, 0x93], "Apple", DeviceClass::Physical),
    a([0x3c, 0x07, 0x54], "Apple", DeviceClass::Physical),
    a([0xa4, 0x83, 0xe7], "Apple", DeviceClass::Physical),
    a([0xf0, 0x18, 0x98], "Apple", DeviceClass::Physical),
    a([0x8c, 0x85, 0x90], "Apple", DeviceClass::Physical),
    a([0x00, 0xe0, 0xfc], "Huawei", DeviceClass::Physical),
    a([0x28, 0x6e, 0xd4], "Huawei", DeviceClass::Physical),
    // --- network gear ------------------------------------------------------
    a([0x00, 0x00, 0x0c], "Cisco", DeviceClass::Network),
    a([0x00, 0x1b, 0xd4], "Cisco", DeviceClass::Network),
    a([0x00, 0x24, 0x97], "Cisco", DeviceClass::Network),
    a([0x00, 0x0b, 0x86], "Aruba", DeviceClass::Network),
    a([0x24, 0xa4, 0x3c], "Ubiquiti", DeviceClass::Network),
    a([0x78, 0x8a, 0x20], "Ubiquiti", DeviceClass::Network),
    a([0xf0, 0x9f, 0xc2], "Ubiquiti", DeviceClass::Network),
    a([0x74, 0x83, 0xc2], "Ubiquiti", DeviceClass::Network),
    a([0x00, 0x0c, 0x42], "MikroTik", DeviceClass::Network),
    a([0x48, 0x8f, 0x5a], "MikroTik", DeviceClass::Network),
    a([0x6c, 0x3b, 0x6b], "MikroTik", DeviceClass::Network),
    a([0xdc, 0x2c, 0x6e], "MikroTik", DeviceClass::Network),
];

/// The built-in table, plus anything loaded from disk.
#[derive(Debug, Clone)]
pub struct OuiDatabase {
    extra: HashMap<[u8; 3], (String, DeviceClass)>,
}

impl Default for OuiDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl OuiDatabase {
    pub fn new() -> Self {
        Self { extra: HashMap::new() }
    }

    /// How many prefixes were loaded from disk on top of the built-ins.
    pub fn loaded(&self) -> usize {
        self.extra.len()
    }

    pub fn built_in() -> usize {
        BUILT_IN.len()
    }

    /// Add one prefix by hand — what the CSV loader and the tests both use.
    pub fn insert(&mut self, prefix: [u8; 3], vendor: impl Into<String>, class: DeviceClass) {
        self.extra.insert(prefix, (vendor.into(), class));
    }

    /// Read a vendor list from disk.
    ///
    /// Two formats, because there are two files people have: IEEE's own
    /// `oui.txt` (`AA-BB-CC   (hex)\t\tVendor Name`) and a plain
    /// `aa:bb:cc,Vendor[,class]` CSV somebody maintains by hand. Detecting
    /// which is a line-by-line decision rather than a mode, so a file holding
    /// both works.
    ///
    /// Unparseable lines are skipped rather than fatal: the IEEE file is full
    /// of headers, blank lines and per-assignment address blocks, and a
    /// network boot server refusing to start over one of them would be absurd.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(Self::parse(&contents))
    }

    pub fn parse(contents: &str) -> Self {
        let mut database = Self::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((prefix, vendor, class)) = parse_ieee(line).or_else(|| parse_csv(line)) {
                database.insert(prefix, vendor, class);
            }
        }
        database
    }

    /// Identify an address.
    ///
    /// A loaded file wins over the built-in table: an operator who went to the
    /// trouble of supplying a vendor list meant it to be authoritative.
    pub fn identify(&self, mac: MacAddr) -> Identification {
        let prefix = mac.oui();
        let locally_administered = mac.is_locally_administered();

        if let Some((vendor, class)) = self.extra.get(&prefix) {
            return Identification {
                vendor: Some(vendor.clone()),
                device_class: *class,
                oui: mac.oui_string(),
                locally_administered,
            };
        }

        if let Some(row) = BUILT_IN.iter().find(|row| row.prefix == prefix) {
            return Identification {
                vendor: Some(row.vendor.to_string()),
                device_class: row.class,
                oui: mac.oui_string(),
                locally_administered,
            };
        }

        Identification {
            vendor: None,
            // A locally administered address that is not one of the well-known
            // hypervisor prefixes is still overwhelmingly a hypervisor, a
            // container or a bond — but "overwhelmingly" is not "is", so this
            // says `unknown` and lets the rule file decide what to do with a
            // machine nobody recognises.
            device_class: DeviceClass::Unknown,
            oui: mac.oui_string(),
            locally_administered,
        }
    }

    /// Every vendor name this database can produce, for the admin UI's filter.
    pub fn vendors(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILT_IN
            .iter()
            .map(|row| row.vendor.to_string())
            .chain(self.extra.values().map(|(vendor, _)| vendor.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// `AA-BB-CC   (hex)<TAB><TAB>Vendor Name` — IEEE's own listing.
fn parse_ieee(line: &str) -> Option<([u8; 3], String, DeviceClass)> {
    let (prefix, rest) = line.split_once("(hex)")?;
    let prefix = parse_prefix(prefix.trim())?;
    let vendor = rest.trim();
    if vendor.is_empty() {
        return None;
    }
    Some((prefix, vendor.to_string(), DeviceClass::Physical))
}

/// `aa:bb:cc,Vendor[,class]` — the hand-maintained shape.
fn parse_csv(line: &str) -> Option<([u8; 3], String, DeviceClass)> {
    let mut fields = line.split(',');
    let prefix = parse_prefix(fields.next()?.trim())?;
    let vendor = fields.next()?.trim();
    if vendor.is_empty() {
        return None;
    }
    let class = match fields.next().map(|field| field.trim().to_ascii_lowercase()).as_deref() {
        Some("virtual") => DeviceClass::Virtual,
        Some("sbc") => DeviceClass::Sbc,
        Some("network") => DeviceClass::Network,
        Some("unknown") => DeviceClass::Unknown,
        _ => DeviceClass::Physical,
    };
    Some((prefix, vendor.to_string(), class))
}

fn parse_prefix(text: &str) -> Option<[u8; 3]> {
    let cleaned: String = text.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();
    if cleaned.len() != 6 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut prefix = [0u8; 3];
    for (index, octet) in prefix.iter_mut().enumerate() {
        *octet = u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(text: &str) -> MacAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_known_prefix_names_its_vendor() {
        let database = OuiDatabase::new();
        let id = database.identify(mac("18:66:da:11:22:33"));
        assert_eq!(id.vendor.as_deref(), Some("Dell"));
        assert_eq!(id.device_class, DeviceClass::Physical);
        assert_eq!(id.oui, "18:66:da");
    }

    #[test]
    fn a_hypervisor_is_identified_as_virtual() {
        let database = OuiDatabase::new();
        for (address, vendor) in [
            ("52:54:00:aa:bb:cc", "QEMU/KVM"),
            ("00:50:56:aa:bb:cc", "VMware"),
            ("00:15:5d:aa:bb:cc", "Microsoft Hyper-V"),
            ("08:00:27:aa:bb:cc", "VirtualBox"),
        ] {
            let id = database.identify(mac(address));
            assert_eq!(id.vendor.as_deref(), Some(vendor), "{address}");
            assert_eq!(id.device_class, DeviceClass::Virtual, "{address}");
        }
    }

    #[test]
    fn a_raspberry_pi_is_a_single_board_computer() {
        let id = OuiDatabase::new().identify(mac("dc:a6:32:11:22:33"));
        assert_eq!(id.vendor.as_deref(), Some("Raspberry Pi"));
        assert_eq!(id.device_class, DeviceClass::Sbc);
    }

    #[test]
    fn an_unassigned_prefix_says_it_does_not_know() {
        // And specifically does not guess. A rule saying `vendor = ["Dell"]`
        // firing for an unrecognised NIC would reimage the wrong machine.
        let id = OuiDatabase::new().identify(mac("aa:bb:cc:dd:ee:ff"));
        assert_eq!(id.vendor, None);
        assert_eq!(id.device_class, DeviceClass::Unknown);
        assert!(id.locally_administered, "aa: bit 1 is set");
    }

    #[test]
    fn a_loaded_list_overrides_the_built_in_table() {
        // The operator supplied a vendor list; they meant it.
        let mut database = OuiDatabase::new();
        database.insert([0x18, 0x66, 0xda], "Dell (retired fleet)", DeviceClass::Physical);
        assert_eq!(
            database.identify(mac("18:66:da:11:22:33")).vendor.as_deref(),
            Some("Dell (retired fleet)")
        );
    }

    #[test]
    fn the_ieee_listing_parses() {
        let database = OuiDatabase::parse(
            "OUI/MA-L                                    Organization\n\
             \n\
             28-CD-C1   (hex)\t\tRaspberry Pi Trading Ltd\n\
             AC-1F-6B   (hex)\t\tSuper Micro Computer, Inc.\n",
        );
        assert_eq!(database.loaded(), 2);
        assert_eq!(
            database.identify(mac("28:cd:c1:00:00:01")).vendor.as_deref(),
            Some("Raspberry Pi Trading Ltd")
        );
    }

    #[test]
    fn a_hand_written_csv_parses_and_can_name_a_class() {
        let database = OuiDatabase::parse(
            "# our own kit\n\
             aa:bb:cc,Bench rig,virtual\n\
             00:11:22,Old switch,network\n\
             de:ad:be,No class given\n",
        );
        assert_eq!(database.identify(mac("aa:bb:cc:00:00:01")).device_class, DeviceClass::Virtual);
        assert_eq!(database.identify(mac("00:11:22:00:00:01")).device_class, DeviceClass::Network);
        assert_eq!(database.identify(mac("de:ad:be:00:00:01")).device_class, DeviceClass::Physical);
    }

    #[test]
    fn junk_in_the_file_is_skipped_rather_than_fatal() {
        // The IEEE file is mostly not assignments. Refusing to boot over one
        // of its headers would be absurd.
        let database = OuiDatabase::parse(
            "\n   \n# comment\nnot a line at all\nAA-BB   (hex)\tToo short\naa:bb:cc,Good\n",
        );
        assert_eq!(database.loaded(), 1);
        assert_eq!(database.identify(mac("aa:bb:cc:00:00:01")).vendor.as_deref(), Some("Good"));
    }

    #[test]
    fn no_two_built_in_rows_claim_the_same_prefix() {
        let mut prefixes: Vec<[u8; 3]> = BUILT_IN.iter().map(|row| row.prefix).collect();
        let before = prefixes.len();
        prefixes.sort_unstable();
        prefixes.dedup();
        assert_eq!(prefixes.len(), before, "a prefix is listed twice, so one row is dead");
    }
}
