//! A hardware address, and the one spelling of it everything else uses.
//!
//! A MAC arrives written four ways — `aa:bb:cc:dd:ee:ff` from a human,
//! `AA-BB-CC-DD-EE-FF` from Windows, `aabbccddeeff` from a URL, and six raw
//! bytes from the wire. All four are the same machine, so the inventory would
//! hold four rows for it unless something decides on one spelling. This does:
//! parsing accepts every form, `Display` emits exactly one, and that is what
//! reaches the database and the rule engine.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 48-bit hardware address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    pub const ZERO: MacAddr = MacAddr([0; 6]);

    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    pub const fn octets(&self) -> [u8; 6] {
        self.0
    }

    /// The first three octets — the part IEEE assigns to a manufacturer.
    pub const fn oui(&self) -> [u8; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }

    /// The OUI as `aa:bb:cc`, which is how a rule file writes it.
    pub fn oui_string(&self) -> String {
        format!("{:02x}:{:02x}:{:02x}", self.0[0], self.0[1], self.0[2])
    }

    /// Whether the address is locally administered (bit 1 of the first octet).
    ///
    /// Worth knowing because it means the OUI is not an assignment and no
    /// vendor lookup will find it: hypervisors, containers and bonded
    /// interfaces all mint addresses in this space.
    pub const fn is_locally_administered(&self) -> bool {
        self.0[0] & 0x02 != 0
    }

    pub const fn is_multicast(&self) -> bool {
        self.0[0] & 0x01 != 0
    }

    pub const fn is_zero(&self) -> bool {
        matches!(self.0, [0, 0, 0, 0, 0, 0])
    }

    /// Twelve lowercase hex digits and nothing else — the URL spelling.
    pub fn hyphenless(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The spelling iPXE uses in `${net0/mac}`, which is also ours.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let octets: [u8; 6] = bytes.get(..6)?.try_into().ok()?;
        Some(Self(octets))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// Why a string is not a hardware address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacParseError(String);

impl fmt::Display for MacParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a MAC address: expected six octets like `aa:bb:cc:dd:ee:ff`", self.0)
    }
}

impl std::error::Error for MacParseError {}

impl FromStr for MacAddr {
    type Err = MacParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let cleaned: String = s.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();

        // Not `chunks(2)` over the bytes: a non-ASCII character would be split
        // mid-codepoint and the error would name a string the user never typed.
        if cleaned.len() != 12 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(MacParseError(s.to_string()));
        }

        let mut octets = [0u8; 6];
        for (index, octet) in octets.iter_mut().enumerate() {
            let pair = &cleaned[index * 2..index * 2 + 2];
            *octet = u8::from_str_radix(pair, 16).map_err(|_| MacParseError(s.to_string()))?;
        }
        Ok(Self(octets))
    }
}

impl Serialize for MacAddr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MacAddr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_parses_to_the_same_address() {
        let expected = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        for spelling in
            ["aa:bb:cc:dd:ee:ff", "AA-BB-CC-DD-EE-FF", "aabbccddeeff", "AABB.CCDD.EEFF", "aa bb cc dd ee ff"]
        {
            assert_eq!(spelling.parse::<MacAddr>().unwrap(), expected, "{spelling}");
        }
    }

    #[test]
    fn there_is_one_spelling_on_the_way_out() {
        let mac: MacAddr = "AA-BB-CC-DD-EE-FF".parse().unwrap();
        assert_eq!(mac.to_string(), "aa:bb:cc:dd:ee:ff");
        assert_eq!(mac.hyphenless(), "aabbccddeeff");
        assert_eq!(mac.oui_string(), "aa:bb:cc");
    }

    #[test]
    fn something_that_is_not_an_address_says_so() {
        for bad in ["", "aa:bb:cc", "gg:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:ff:00", "hello"] {
            assert!(bad.parse::<MacAddr>().is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn a_multibyte_character_is_rejected_rather_than_split() {
        // The length check runs on the filtered string, so a character wider
        // than a byte cannot land the slice in the middle of a codepoint.
        assert!("aa:bb:cc:dd:ee:ff£".parse::<MacAddr>().is_err());
        assert!("ααββγγδδεεζζ".parse::<MacAddr>().is_err());
    }

    #[test]
    fn a_hypervisor_address_is_recognised_as_locally_administered() {
        // QEMU's 52:54:00 and Docker's 02:42:… both have bit 1 set.
        assert!("52:54:00:12:34:56".parse::<MacAddr>().unwrap().is_locally_administered());
        assert!(!"18:66:da:11:22:33".parse::<MacAddr>().unwrap().is_locally_administered());
    }

    #[test]
    fn it_round_trips_through_json() {
        let mac: MacAddr = "18:66:da:11:22:33".parse().unwrap();
        let json = serde_json::to_string(&mac).unwrap();
        assert_eq!(json, "\"18:66:da:11:22:33\"");
        assert_eq!(serde_json::from_str::<MacAddr>(&json).unwrap(), mac);
    }
}
