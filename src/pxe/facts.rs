//! Everything known about the machine that is asking to boot.
//!
//! Two very different conversations produce one of these. The first is a DHCP
//! packet, where all we have is what the firmware volunteered: a hardware
//! address, an architecture number, a vendor class string. The second is an
//! HTTP request from iPXE after it has read the machine's SMBIOS tables, where
//! we additionally know the manufacturer, the product name, the serial and the
//! asset tag.
//!
//! Both arrive here as the same struct, with the fields the earlier stage
//! cannot know left as `None` — because a rule matching on `product` should
//! simply not fire during the DHCP stage, rather than fire on a guess.

use std::net::{IpAddr, Ipv4Addr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::arch::ClientArch;
use super::mac::MacAddr;
use super::oui::{DeviceClass, OuiDatabase};

/// Which half of the boot this is.
///
/// It matters because the answer has a different *shape*: the firmware stage
/// is answered with a file name to fetch, and the iPXE stage with a script to
/// run. A rule set does not usually care, but the engine does, and `pxe:test`
/// prints it so an operator can see which question they are asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// The machine's own PXE/HTTP boot firmware, over DHCP.
    Firmware,
    /// iPXE, over HTTP, asking for its script.
    Ipxe,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Firmware => "firmware",
            Stage::Ipxe => "ipxe",
        }
    }
}

/// DHCP option 77 as this project's own iPXE sends it (`ipxe/embed.ipxe`).
///
/// It starts with `iPXE` on purpose: everything that recognises iPXE by its
/// user class — this server's own check, and the prefix matches dnsmasq and
/// ISC configurations use — keeps recognising it. The suffix is only ever an
/// extra signal.
pub const OWN_IPXE_USER_CLASS: &str = "iPXE-kindling";

/// What builds from before the project was renamed send, still recognised so
/// loaders already in service keep being told apart until they are rebuilt.
pub const LEGACY_OWN_IPXE_USER_CLASS: &str = "iPXE-safewords";

/// Which iPXE is asking, as far as the request can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IpxeBuild {
    /// Not iPXE at all: the machine's own firmware.
    NotIpxe,
    /// iPXE, but nothing says whose build. Usually a stock binary; also what
    /// every request over HTTP looks like, since that path does not carry the
    /// user class.
    Unidentified,
    /// This project's build, which knows where the server is and never
    /// follows a DHCP boot filename back into itself.
    Own,
}

impl IpxeBuild {
    pub fn as_str(&self) -> &'static str {
        match self {
            IpxeBuild::NotIpxe => "not-ipxe",
            IpxeBuild::Unidentified => "unidentified",
            IpxeBuild::Own => "own",
        }
    }
}

/// The facts a rule may match on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFacts {
    pub mac: MacAddr,
    pub arch: ClientArch,
    pub stage: Stage,

    // --- straight off the wire ---------------------------------------------
    /// DHCP option 60. `PXEClient:Arch:00007:UNDI:003001` and friends.
    pub vendor_class: Option<String>,
    /// DHCP option 77. iPXE puts `iPXE` here, which is how the loop ends.
    pub user_class: Option<String>,
    /// Whether the request carried DHCP option 175 — iPXE's own encapsulated
    /// options, which only iPXE sends.
    pub ipxe_options: bool,
    /// DHCP option 12.
    pub hostname: Option<String>,
    /// DHCP option 97 — the SMBIOS system UUID, as the firmware spells it.
    pub uuid: Option<String>,
    pub client_ip: Option<IpAddr>,
    /// `giaddr`: the relay that forwarded this, if the client is off-segment.
    pub relay_ip: Option<Ipv4Addr>,
    /// The DHCP transaction id, when this came from a packet.
    ///
    /// Not a fact about the machine — a fact about the *conversation*, and the
    /// only thing that separates a client retransmitting one request from a
    /// client that has been round the loop again. No rule matches on it; the
    /// loop breaker does.
    pub transaction: Option<u32>,

    // --- only iPXE knows these ---------------------------------------------
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub asset: Option<String>,
    /// iPXE's `${platform}`: `pcbios` or `efi`.
    pub platform: Option<String>,

    // --- derived -----------------------------------------------------------
    /// The OUI assignee, or the SMBIOS manufacturer when the OUI is unknown.
    pub vendor: Option<String>,
    pub device_class: DeviceClass,
    pub oui: String,

    // --- from the inventory ------------------------------------------------
    /// Tags this machine carries, from the database.
    pub tags: Vec<String>,
    /// Whether this server has seen this machine before. The difference
    /// between "image every new machine" and "image every machine, forever".
    pub known: bool,
    /// How many times it has booted through here.
    pub boot_count: u64,

    pub at: DateTime<Utc>,
}

impl ClientFacts {
    pub fn new(mac: MacAddr, arch: ClientArch, stage: Stage) -> Self {
        Self {
            mac,
            arch,
            stage,
            vendor_class: None,
            user_class: None,
            ipxe_options: false,
            hostname: None,
            uuid: None,
            client_ip: None,
            relay_ip: None,
            transaction: None,
            manufacturer: None,
            product: None,
            serial: None,
            asset: None,
            platform: None,
            vendor: None,
            device_class: DeviceClass::Unknown,
            oui: mac.oui_string(),
            tags: Vec::new(),
            known: false,
            boot_count: 0,
            at: Utc::now(),
        }
    }

    /// Fill in `vendor` and `device_class` from the hardware address.
    ///
    /// The OUI wins over the SMBIOS manufacturer where both exist, because the
    /// OUI describes the NIC that is actually talking to us — on a machine
    /// with an add-in card those genuinely differ, and the one that matters
    /// for a driver decision is the card.
    #[must_use = "this returns the identified facts rather than identifying in place"]
    pub fn identified(mut self, ouis: &OuiDatabase) -> Self {
        let identification = ouis.identify(self.mac);
        self.oui = identification.oui;
        self.device_class = identification.device_class;
        self.vendor = identification.vendor.or_else(|| self.manufacturer.clone());
        self
    }

    /// Whether iPXE is the thing asking.
    ///
    /// Read from the user class rather than assumed from the stage, because
    /// during DHCP this is precisely the question being answered: an iPXE that
    /// is handed iPXE again boots in a loop until somebody notices.
    pub fn is_ipxe(&self) -> bool {
        if self.stage == Stage::Ipxe || self.ipxe_options {
            return true;
        }
        self.user_class.as_deref().is_some_and(|class| class.to_lowercase().contains("ipxe"))
    }

    /// Whose iPXE this is, when it is iPXE.
    ///
    /// Purely additive: nothing that decides whether to hand out a loader or
    /// a script reads this — `is_ipxe` does, and it answers `true` for both
    /// builds. It is here so a log line, `pxe:test` or a future rule can say
    /// which one a machine is running, which is the first question when a
    /// boot misbehaves.
    pub fn ipxe_build(&self) -> IpxeBuild {
        if !self.is_ipxe() {
            return IpxeBuild::NotIpxe;
        }
        match self.user_class.as_deref() {
            Some(class)
                if class.trim().eq_ignore_ascii_case(OWN_IPXE_USER_CLASS)
                    || class.trim().eq_ignore_ascii_case(LEGACY_OWN_IPXE_USER_CLASS) =>
            {
                IpxeBuild::Own
            }
            _ => IpxeBuild::Unidentified,
        }
    }

    /// Whether the firmware asked over HTTP rather than TFTP.
    pub fn is_http_boot(&self) -> bool {
        self.arch.is_http_boot()
            || self
                .vendor_class
                .as_deref()
                .is_some_and(|class| class.to_uppercase().starts_with("HTTPCLIENT"))
    }

    /// The address a subnet rule is tested against: the relay if there was
    /// one, otherwise the client's own address.
    ///
    /// The relay comes first on purpose. A machine that has not been given an
    /// address yet has no address of its own, and the relay's is the only
    /// thing that says which network it is on.
    pub fn network_address(&self) -> Option<IpAddr> {
        self.relay_ip.map(IpAddr::V4).or(self.client_ip)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|held| held.eq_ignore_ascii_case(tag))
    }

    /// A one-line description for a log or a table.
    pub fn summary(&self) -> String {
        let vendor = self.vendor.as_deref().unwrap_or("unknown vendor");
        let name = self
            .product
            .as_deref()
            .or(self.hostname.as_deref())
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        format!("{} — {vendor}{name}, {}", self.mac, self.arch.label())
    }

    // --- builders, for the wire decoders and for tests ---------------------

    #[must_use]
    pub fn with_vendor_class(mut self, value: Option<String>) -> Self {
        self.vendor_class = value;
        self
    }

    #[must_use]
    pub fn with_user_class(mut self, value: Option<String>) -> Self {
        self.user_class = value;
        self
    }

    #[must_use]
    pub fn with_ipxe_options(mut self, present: bool) -> Self {
        self.ipxe_options = present;
        self
    }

    #[must_use]
    pub fn with_hostname(mut self, value: Option<String>) -> Self {
        self.hostname = value;
        self
    }

    #[must_use]
    pub fn with_uuid(mut self, value: Option<String>) -> Self {
        self.uuid = value;
        self
    }

    #[must_use]
    pub fn with_client_ip(mut self, value: Option<IpAddr>) -> Self {
        self.client_ip = value;
        self
    }

    #[must_use]
    pub fn with_relay_ip(mut self, value: Option<Ipv4Addr>) -> Self {
        self.relay_ip = value;
        self
    }

    #[must_use]
    pub fn with_transaction(mut self, xid: Option<u32>) -> Self {
        self.transaction = xid;
        self
    }

    #[must_use]
    pub fn with_smbios(
        mut self,
        manufacturer: Option<String>,
        product: Option<String>,
        serial: Option<String>,
        asset: Option<String>,
    ) -> Self {
        self.manufacturer = manufacturer;
        self.product = product;
        self.serial = serial;
        self.asset = asset;
        self
    }

    #[must_use]
    pub fn with_platform(mut self, value: Option<String>) -> Self {
        self.platform = value;
        self
    }

    #[must_use]
    pub fn with_inventory(mut self, tags: Vec<String>, known: bool, boot_count: u64) -> Self {
        self.tags = tags;
        self.known = known;
        self.boot_count = boot_count;
        self
    }

    #[must_use]
    pub fn at(mut self, when: DateTime<Utc>) -> Self {
        self.at = when;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ClientFacts {
        ClientFacts::new("18:66:da:11:22:33".parse().unwrap(), ClientArch::X64_UEFI, Stage::Firmware)
    }

    #[test]
    fn the_oui_fills_in_the_vendor() {
        let identified = facts().identified(&OuiDatabase::new());
        assert_eq!(identified.vendor.as_deref(), Some("Dell"));
        assert_eq!(identified.device_class, DeviceClass::Physical);
    }

    #[test]
    fn an_unknown_card_falls_back_to_what_smbios_said() {
        let unknown = ClientFacts::new(
            "aa:bb:cc:dd:ee:ff".parse().unwrap(),
            ClientArch::X64_UEFI,
            Stage::Ipxe,
        )
        .with_smbios(Some("Framework".into()), Some("Laptop 13".into()), None, None)
        .identified(&OuiDatabase::new());

        assert_eq!(unknown.vendor.as_deref(), Some("Framework"));
    }

    #[test]
    fn the_card_wins_over_smbios_when_both_are_known() {
        // A Dell chassis with an Intel add-in card is an Intel NIC talking to
        // us, and the NIC is what a driver decision turns on.
        let both = facts()
            .with_smbios(Some("Dell Inc.".into()), None, None, None)
            .identified(&OuiDatabase::new());
        assert_eq!(both.vendor.as_deref(), Some("Dell"));
    }

    #[test]
    fn ipxe_is_recognised_from_the_user_class_during_dhcp() {
        // This is the check that ends the chain. Without it, iPXE is handed
        // iPXE and the machine boots in a loop.
        let ipxe = facts().with_user_class(Some("iPXE".into()));
        assert!(ipxe.is_ipxe());

        let firmware = facts().with_user_class(Some("PXEClient".into()));
        assert!(!firmware.is_ipxe());

        assert!(!facts().is_ipxe(), "no user class at all is firmware");
    }

    #[test]
    fn option_175_identifies_ipxe_even_with_no_user_class_at_all() {
        // The second signal, and the reason it is here: a build without
        // `DHCP_CLIENT_USER_CLASS`, or a relay that drops option 77, would
        // otherwise be handed iPXE by iPXE for ever.
        let quiet = facts().with_ipxe_options(true);
        assert!(quiet.is_ipxe());
        assert!(!facts().is_ipxe(), "and its absence still means firmware");
    }

    #[test]
    fn this_projects_own_build_is_still_ipxe_to_every_existing_check() {
        // The suffix is an extra signal, never a replacement: a build that
        // stopped being recognised as iPXE would be handed iPXE again.
        let own = facts().with_user_class(Some(OWN_IPXE_USER_CLASS.into()));
        assert!(own.is_ipxe());
        assert_eq!(own.ipxe_build(), IpxeBuild::Own);

        // Case and stray whitespace do not matter, the same as `is_ipxe`.
        let shouted = facts().with_user_class(Some(" IPXE-KINDLING ".into()));
        assert_eq!(shouted.ipxe_build(), IpxeBuild::Own);

        // A build from before the rename is still ours.
        let legacy = facts().with_user_class(Some(LEGACY_OWN_IPXE_USER_CLASS.into()));
        assert_eq!(legacy.ipxe_build(), IpxeBuild::Own);
    }

    #[test]
    fn a_stock_build_or_an_unlabelled_one_is_ipxe_of_unknown_origin() {
        let stock = facts().with_user_class(Some("iPXE".into()));
        assert_eq!(stock.ipxe_build(), IpxeBuild::Unidentified);

        // Option 175 alone says iPXE, and says nothing about whose.
        assert_eq!(facts().with_ipxe_options(true).ipxe_build(), IpxeBuild::Unidentified);

        // Something that merely contains our suffix is not our build.
        let lookalike = facts().with_user_class(Some("iPXE-kindling-fork".into()));
        assert_eq!(lookalike.ipxe_build(), IpxeBuild::Unidentified);
    }

    #[test]
    fn firmware_is_not_any_ipxe_even_if_it_claims_our_suffix_without_the_name() {
        assert_eq!(facts().ipxe_build(), IpxeBuild::NotIpxe);
        let odd = facts().with_user_class(Some("kindling".into()));
        assert_eq!(odd.ipxe_build(), IpxeBuild::NotIpxe);
    }

    #[test]
    fn the_http_stage_is_always_ipxe_whatever_it_claims() {
        let http = ClientFacts::new(MacAddr::ZERO, ClientArch::BIOS, Stage::Ipxe);
        assert!(http.is_ipxe());
    }

    #[test]
    fn http_boot_is_read_from_the_architecture_or_the_vendor_class() {
        assert!(ClientFacts::new(MacAddr::ZERO, ClientArch::X64_UEFI_HTTP, Stage::Firmware)
            .is_http_boot());

        // Some firmware sends a TFTP architecture and an HTTPClient vendor
        // class. It still wants a URL.
        assert!(facts().with_vendor_class(Some("HTTPClient:Arch:00016:UNDI:003001".into()))
            .is_http_boot());

        assert!(!facts().is_http_boot());
    }

    #[test]
    fn a_relay_decides_the_network_before_the_client_address_does() {
        // A machine mid-DISCOVER has no address of its own; the relay's is the
        // only evidence of which network it is on.
        let relayed = facts()
            .with_client_ip(Some("0.0.0.0".parse().unwrap()))
            .with_relay_ip(Some(Ipv4Addr::new(10, 20, 0, 1)));
        assert_eq!(relayed.network_address(), Some("10.20.0.1".parse().unwrap()));

        let direct = facts().with_client_ip(Some("192.168.1.50".parse().unwrap()));
        assert_eq!(direct.network_address(), Some("192.168.1.50".parse().unwrap()));
    }

    #[test]
    fn tags_are_matched_without_regard_to_case() {
        let tagged = facts().with_inventory(vec!["Lab".into()], true, 3);
        assert!(tagged.has_tag("lab"));
        assert!(tagged.has_tag("LAB"));
        assert!(!tagged.has_tag("prod"));
    }
}
