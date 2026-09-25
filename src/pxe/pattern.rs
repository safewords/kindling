//! The two comparisons a rule file is written in: globs and subnets.
//!
//! A rule says `product = ["OptiPlex*"]` and `subnet = ["10.20.0.0/16"]`, and
//! both have to mean the obvious thing to whoever wrote them. Neither is a
//! regular expression, deliberately: a rule file is read by people who are
//! reasoning about hardware, and `.*` in it would be a silent superset of what
//! they meant the first time somebody wrote `10.0.0.1` as a "pattern".

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A shell-style glob: `*` spans any run of characters, `?` exactly one.
///
/// Matching is case-insensitive, because the things being matched — vendor
/// classes, product names, hostnames — arrive with whatever capitalisation the
/// firmware vendor felt like on the day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    source: String,
    lowered: String,
}

impl Glob {
    pub fn new(pattern: impl Into<String>) -> Self {
        let source = pattern.into();
        let lowered = source.to_lowercase();
        Self { source, lowered }
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether this pattern contains no wildcards, i.e. is a plain equality.
    pub fn is_literal(&self) -> bool {
        !self.source.contains(['*', '?'])
    }

    pub fn matches(&self, text: &str) -> bool {
        glob_match(&self.lowered, &text.to_lowercase())
    }

    pub fn matches_option(&self, text: Option<&str>) -> bool {
        text.is_some_and(|text| self.matches(text))
    }
}

impl fmt::Display for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl From<&str> for Glob {
    fn from(value: &str) -> Self {
        Glob::new(value)
    }
}

impl Serialize for Glob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl<'de> Deserialize<'de> for Glob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Glob::new)
    }
}

/// Iterative glob matching with one backtrack point.
///
/// Recursion here would be a stack depth an attacker picks: the pattern comes
/// from a file an operator wrote, but the *text* comes off the wire, and
/// `a*a*a*a*a*b` against a long run of `a` is the classic way to turn a
/// matcher into a hang. Carrying one restart position makes it linear.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();

    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut restart) = (None::<usize>, 0usize);

    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                restart = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                // Backtrack: the last `*` swallows one more character.
                Some(position) => {
                    p = position + 1;
                    restart += 1;
                    t = restart;
                }
                None => return false,
            },
        }
    }

    // Trailing `*`s match the empty remainder.
    while matches!(pattern.get(p), Some('*')) {
        p += 1;
    }
    p == pattern.len()
}

/// An IP network in CIDR notation, or a single address.
///
/// Both families, because the admin interface is reached over whichever one
/// the operator's browser picked and a rule matching `::1` should work in a
/// test. DHCP itself is v4 here — DHCPv6 network boot is a different protocol
/// and this server does not speak it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn new(network: IpAddr, prefix: u8) -> Result<Self, CidrParseError> {
        let max = match network {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > max {
            return Err(CidrParseError(format!("/{prefix} is wider than the address family allows")));
        }
        Ok(Self { network: masked(network, prefix), prefix })
    }

    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                masked(address, self.prefix) == self.network
            }
            // A v4 rule does not match a v6 client. Saying "no" here is the
            // conservative answer: the alternative is a subnet rule that fires
            // for a machine on a network the operator never wrote down.
            _ => false,
        }
    }

    pub fn contains_v4(&self, address: Ipv4Addr) -> bool {
        self.contains(IpAddr::V4(address))
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }
}

fn masked(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            IpAddr::V4(Ipv4Addr::from(bits & mask))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
            IpAddr::V6(Ipv6Addr::from(bits & mask))
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CidrParseError(String);

impl fmt::Display for CidrParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CidrParseError {}

impl FromStr for Cidr {
    type Err = CidrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        match s.split_once('/') {
            Some((address, prefix)) => {
                let address: IpAddr = address
                    .parse()
                    .map_err(|_| CidrParseError(format!("`{address}` is not an IP address")))?;
                let prefix: u8 = prefix
                    .parse()
                    .map_err(|_| CidrParseError(format!("`{prefix}` is not a prefix length")))?;
                Cidr::new(address, prefix)
            }
            // A bare address is a host route, which is what somebody writing
            // one address in a list means.
            None => {
                let address: IpAddr =
                    s.parse().map_err(|_| CidrParseError(format!("`{s}` is not an IP address or CIDR block")))?;
                let prefix = if address.is_ipv4() { 32 } else { 128 };
                Cidr::new(address, prefix)
            }
        }
    }
}

impl Serialize for Cidr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_matches_only_itself() {
        let glob = Glob::new("OptiPlex 7090");
        assert!(glob.matches("OptiPlex 7090"));
        assert!(!glob.matches("OptiPlex 7090 Ultra"));
        assert!(glob.is_literal());
    }

    #[test]
    fn matching_ignores_case_because_firmware_does_not_agree_on_it() {
        assert!(Glob::new("optiplex*").matches("OptiPlex 7090"));
        assert!(Glob::new("PXEClient:Arch:*").matches("pxeclient:arch:00007:undi:003001"));
    }

    #[test]
    fn a_star_spans_anything_including_nothing() {
        let glob = Glob::new("lab-*");
        assert!(glob.matches("lab-01"));
        assert!(glob.matches("lab-"));
        assert!(!glob.matches("prod-01"));
        assert!(Glob::new("*").matches(""));
        assert!(Glob::new("*pxe*").matches("ipxe"));
    }

    #[test]
    fn a_question_mark_is_exactly_one_character() {
        let glob = Glob::new("rack-?");
        assert!(glob.matches("rack-1"));
        assert!(!glob.matches("rack-12"));
        assert!(!glob.matches("rack-"));
    }

    #[test]
    fn a_pathological_pattern_does_not_hang() {
        // The reason the matcher is iterative. Before, this shape was the way
        // to make a rule evaluation take exponential time on a string the
        // client chose.
        let text = "a".repeat(64);
        assert!(!Glob::new("a*a*a*a*a*a*a*b").matches(&text));
    }

    #[test]
    fn an_absent_value_matches_nothing_at_all() {
        // Not even `*`. A rule asking about a product name should not fire for
        // a machine that never told us one — that is the difference between
        // "any product" and "no such field".
        assert!(!Glob::new("*").matches_option(None));
        assert!(Glob::new("*").matches_option(Some("anything")));
    }

    #[test]
    fn a_subnet_contains_its_own_addresses_and_no_others() {
        let cidr: Cidr = "10.20.0.0/16".parse().unwrap();
        assert!(cidr.contains_v4(Ipv4Addr::new(10, 20, 5, 7)));
        assert!(cidr.contains_v4(Ipv4Addr::new(10, 20, 255, 254)));
        assert!(!cidr.contains_v4(Ipv4Addr::new(10, 21, 0, 1)));
    }

    #[test]
    fn a_bare_address_is_a_host_route() {
        let cidr: Cidr = "192.168.1.5".parse().unwrap();
        assert_eq!(cidr.prefix(), 32);
        assert!(cidr.contains_v4(Ipv4Addr::new(192, 168, 1, 5)));
        assert!(!cidr.contains_v4(Ipv4Addr::new(192, 168, 1, 6)));
    }

    #[test]
    fn a_block_is_normalised_to_its_network_address() {
        // `10.0.0.99/24` is how people write it and `10.0.0.0/24` is what they
        // mean; printing it back the second way is how they see we agreed.
        let cidr: Cidr = "10.0.0.99/24".parse().unwrap();
        assert_eq!(cidr.to_string(), "10.0.0.0/24");
    }

    #[test]
    fn a_zero_prefix_is_everything_of_its_family() {
        let any: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(any.contains_v4(Ipv4Addr::new(1, 2, 3, 4)));
        assert!(!any.contains("::1".parse().unwrap()), "a v4 rule is not a v6 rule");
    }

    #[test]
    fn both_families_work_and_do_not_cross() {
        let v6: Cidr = "2001:db8::/32".parse().unwrap();
        assert!(v6.contains("2001:db8::1".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        assert!(!v6.contains("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        for bad in ["", "10.0.0.0/33", "not-an-ip/24", "10.0.0.0/abc", "banana"] {
            assert!(bad.parse::<Cidr>().is_err(), "`{bad}` should not parse");
        }
    }
}
