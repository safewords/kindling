//! A machine this server has seen.
//!
//! The inventory is the *observed* half of the system: rows appear because a
//! machine asked to boot, never because somebody declared it. That is what
//! makes `known = false` a usable rule condition — and see `overrides` for why
//! "new" is the boot counter rather than the row's existence.
//!
//! Two columns here are an operator's, not an observation's: `pinned_profile`
//! and `once_profile`. They live beside the facts rather than in the rule file
//! because they are typed at 3am against one machine, and the rule file is
//! reviewed.

use chrono::{DateTime, Utc};
use rainier_framework::prelude::*;
use rainier_orm::Json;

use crate::pxe::facts::ClientFacts;
use crate::pxe::mac::MacAddr;
use crate::pxe::policy::Overrides;

#[derive(Entity, Clone, Debug)]
#[orm(table = "hosts")]
#[orm(index = "last_seen")]
pub struct Host {
    #[orm(pk, auto_increment)]
    pub id: u64,

    /// The one spelling, lowercase and colon-separated. Unique, because this
    /// is the identity of the row and four spellings of it would be four rows
    /// for one machine.
    #[orm(unique)]
    pub mac: String,

    pub hostname: Option<String>,
    pub vendor: Option<String>,
    pub device_class: String,
    /// The architecture label last seen, e.g. `x64-uefi`.
    pub arch: String,
    pub uuid: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub asset: Option<String>,
    pub last_ip: Option<String>,

    /// Free-form labels a rule can match on, stored as JSON text so the same
    /// column works on SQLite, MySQL and Postgres.
    pub tags: Json<Vec<String>>,

    /// Always boots this, whatever the rules say.
    pub pinned_profile: Option<String>,
    /// Boots this once, then the column is cleared.
    pub once_profile: Option<String>,
    /// What it booted last time, for the inventory table.
    pub last_profile: Option<String>,

    pub boot_count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Model for Host {
    /// A machine is addressed by its hardware address, so `/api/hosts/{host}`
    /// takes a MAC rather than a row id nobody can read off a sticker.
    fn route_key_name() -> &'static str {
        "mac"
    }
}

impl Host {
    /// A row for a machine seen for the first time.
    pub fn first_sighting(facts: &ClientFacts) -> Self {
        Self {
            id: 0,
            mac: facts.mac.to_string(),
            hostname: facts.hostname.clone(),
            vendor: facts.vendor.clone(),
            device_class: facts.device_class.as_str().to_string(),
            arch: facts.arch.label(),
            uuid: facts.uuid.clone(),
            manufacturer: facts.manufacturer.clone(),
            product: facts.product.clone(),
            serial: facts.serial.clone(),
            asset: facts.asset.clone(),
            last_ip: facts.client_ip.map(|ip| ip.to_string()),
            tags: Json(Vec::new()),
            pinned_profile: None,
            once_profile: None,
            last_profile: None,
            boot_count: 0,
            first_seen: facts.at,
            last_seen: facts.at,
        }
    }

    /// Fold in what a later sighting learned.
    ///
    /// A field is only overwritten when the new sighting actually carries one.
    /// The DHCP stage knows no product name and the iPXE stage knows no relay,
    /// so a blind overwrite would have each stage erasing what the other
    /// learned — and the inventory would show whichever stage happened last.
    pub fn observe(&mut self, facts: &ClientFacts) {
        if facts.hostname.is_some() {
            self.hostname.clone_from(&facts.hostname);
        }
        if facts.vendor.is_some() {
            self.vendor.clone_from(&facts.vendor);
        }
        if facts.uuid.is_some() {
            self.uuid.clone_from(&facts.uuid);
        }
        if facts.manufacturer.is_some() {
            self.manufacturer.clone_from(&facts.manufacturer);
        }
        if facts.product.is_some() {
            self.product.clone_from(&facts.product);
        }
        if facts.serial.is_some() {
            self.serial.clone_from(&facts.serial);
        }
        if facts.asset.is_some() {
            self.asset.clone_from(&facts.asset);
        }
        if let Some(ip) = facts.client_ip {
            self.last_ip = Some(ip.to_string());
        }
        if facts.device_class != crate::pxe::oui::DeviceClass::Unknown {
            self.device_class = facts.device_class.as_str().to_string();
        }
        self.arch = facts.arch.label();
        self.last_seen = facts.at;
    }

    /// What the rule engine needs to know about this machine.
    ///
    /// `known` is **"has this machine ever been served a boot script here"**,
    /// not "is there a row for it". The distinction is the whole reason a rule
    /// like `known = false` works at all: one boot is three or four separate
    /// requests, the row is created by the first of them, and the request that
    /// actually picks an image is the last. Defining it by the row's existence
    /// would make every machine known by the time the decision mattered, and
    /// the rule would silently never fire.
    pub fn overrides(&self) -> Overrides {
        Overrides {
            tags: self.tags.0.clone(),
            known: self.boot_count > 0,
            boot_count: self.boot_count,
            pinned_profile: self.pinned_profile.clone(),
            once_profile: self.once_profile.clone(),
        }
    }

    pub fn address(&self) -> Option<MacAddr> {
        self.mac.parse().ok()
    }

    /// The facts this machine would present at the iPXE stage, as far as the
    /// inventory remembers them — what a policy is tried against when the
    /// question is "what would this change do to the machines I have".
    ///
    /// The iPXE stage, because that is where the decision that matters is
    /// made and where the SMBIOS facts are known. `known` and the boot count
    /// are the machine's as they stand, so a `known = false` rule previews as
    /// firing only for machines that have never booted here.
    pub fn facts(&self, ouis: &crate::pxe::oui::OuiDatabase) -> Option<ClientFacts> {
        let mac = self.address()?;
        let arch = crate::pxe::arch::parse_arch(&self.arch)
            .unwrap_or(crate::pxe::arch::ClientArch::X64_UEFI);
        let overrides = self.overrides();

        let mut facts = ClientFacts::new(mac, arch, crate::pxe::facts::Stage::Ipxe)
            .with_hostname(self.hostname.clone())
            .with_uuid(self.uuid.clone())
            .with_smbios(
                self.manufacturer.clone(),
                self.product.clone(),
                self.serial.clone(),
                self.asset.clone(),
            )
            .with_client_ip(self.last_ip.as_deref().and_then(|ip| ip.parse().ok()))
            .identified(ouis)
            .with_inventory(overrides.tags, overrides.known, overrides.boot_count);

        // What was observed beats what the address alone suggests.
        if facts.vendor.is_none() {
            facts.vendor.clone_from(&self.vendor);
        }
        Some(facts)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.0.iter().any(|held| held.eq_ignore_ascii_case(tag))
    }

    /// Add a tag, keeping the list a set and its order stable.
    pub fn add_tag(&mut self, tag: &str) -> bool {
        if self.has_tag(tag) {
            return false;
        }
        self.tags.0.push(tag.to_string());
        true
    }

    pub fn remove_tag(&mut self, tag: &str) -> bool {
        let before = self.tags.0.len();
        self.tags.0.retain(|held| !held.eq_ignore_ascii_case(tag));
        self.tags.0.len() != before
    }

    /// Merge in tags a rule attached, reporting whether anything changed.
    pub fn apply_tags(&mut self, tags: &[String]) -> bool {
        let mut changed = false;
        for tag in tags {
            changed |= self.add_tag(tag);
        }
        changed
    }

    /// The API and the admin UI's shape.
    ///
    /// Written out rather than derived, because `Json<Vec<String>>` is a
    /// storage type and `["lab", "dell"]` is what an API client wants.
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "mac": self.mac,
            "hostname": self.hostname,
            "vendor": self.vendor,
            "device_class": self.device_class,
            "arch": self.arch,
            "uuid": self.uuid,
            "manufacturer": self.manufacturer,
            "product": self.product,
            "serial": self.serial,
            "asset": self.asset,
            "last_ip": self.last_ip,
            "tags": self.tags.0,
            "pinned_profile": self.pinned_profile,
            "once_profile": self.once_profile,
            "last_profile": self.last_profile,
            "boot_count": self.boot_count,
            "first_seen": self.first_seen,
            "last_seen": self.last_seen,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::facts::Stage;
    use crate::pxe::oui::OuiDatabase;

    fn facts() -> ClientFacts {
        ClientFacts::new("18:66:da:11:22:33".parse().unwrap(), ClientArch::X64_UEFI, Stage::Firmware)
            .identified(&OuiDatabase::new())
    }

    #[test]
    fn a_first_sighting_records_what_the_packet_knew() {
        let host = Host::first_sighting(&facts());
        assert_eq!(host.mac, "18:66:da:11:22:33");
        assert_eq!(host.vendor.as_deref(), Some("Dell"));
        assert_eq!(host.arch, "x64-uefi");
        assert_eq!(host.boot_count, 0, "it has not booted yet, it has been seen");
        assert!(host.tags.0.is_empty());
    }

    #[test]
    fn a_later_stage_adds_what_it_knows_without_erasing_the_rest() {
        // The bug this method exists to avoid: the DHCP stage knows no product
        // name, so a blind overwrite would erase what iPXE reported a second
        // earlier, and the inventory would show whichever stage ran last.
        let mut host = Host::first_sighting(&facts());
        host.hostname = Some("lab-01".into());

        let from_ipxe = ClientFacts::new(
            "18:66:da:11:22:33".parse().unwrap(),
            ClientArch::X64_UEFI,
            Stage::Ipxe,
        )
        .with_smbios(Some("Dell Inc.".into()), Some("OptiPlex 7090".into()), None, None);

        host.observe(&from_ipxe);

        assert_eq!(host.product.as_deref(), Some("OptiPlex 7090"), "the new fact landed");
        assert_eq!(host.hostname.as_deref(), Some("lab-01"), "the old one survived");
    }

    #[test]
    fn a_tag_list_behaves_like_a_set() {
        let mut host = Host::first_sighting(&facts());
        assert!(host.add_tag("lab"));
        assert!(!host.add_tag("LAB"), "already there, in a different case");
        assert_eq!(host.tags.0, vec!["lab"]);

        assert!(host.has_tag("Lab"));
        assert!(host.remove_tag("LAB"));
        assert!(!host.remove_tag("lab"));
        assert!(host.tags.0.is_empty());
    }

    #[test]
    fn applying_rule_tags_reports_whether_a_write_is_needed() {
        // The inventory is written on every boot of every machine in a rack.
        // Knowing nothing changed is what keeps that from being a write.
        let mut host = Host::first_sighting(&facts());
        assert!(host.apply_tags(&["lab".into(), "dell".into()]));
        assert!(!host.apply_tags(&["lab".into()]), "nothing new: no write");
    }

    #[test]
    fn a_machine_stays_new_until_it_has_actually_been_given_something_to_boot() {
        // One boot is three or four requests. The row appears on the first of
        // them and the image is chosen on the last, so "new" has to survive
        // the whole conversation or `known = false` chooses nothing, ever.
        let mut host = Host::first_sighting(&facts());
        assert!(!host.overrides().known, "seen, but never booted: still new");

        host.boot_count = 1;
        assert!(host.overrides().known, "it has booted here now");
    }

    #[test]
    fn a_known_host_answers_with_its_overrides() {
        let mut host = Host::first_sighting(&facts());
        host.boot_count = 4;
        host.pinned_profile = Some("local".into());
        host.add_tag("lab");

        let overrides = host.overrides();
        assert!(overrides.known);
        assert_eq!(overrides.boot_count, 4);
        assert_eq!(overrides.pinned_profile.as_deref(), Some("local"));
        assert_eq!(overrides.tags, vec!["lab"]);
    }

    #[test]
    fn the_route_key_is_the_address_on_the_sticker() {
        assert_eq!(Host::route_key_name(), "mac");
        assert_eq!(Host::primary_key(), "id");
    }

    #[test]
    fn the_api_shape_unwraps_the_stored_json() {
        let mut host = Host::first_sighting(&facts());
        host.add_tag("lab");
        let json = host.as_json();
        assert_eq!(json["tags"], serde_json::json!(["lab"]));
        assert_eq!(json["mac"], "18:66:da:11:22:33");
    }
}
