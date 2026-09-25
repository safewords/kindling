//! One line of the boot log.
//!
//! A network boot is three or four conversations with three different
//! protocols, and when it goes wrong the question is always "how far did it
//! get" — did the machine get an offer, did it fetch the loader, did it ask
//! for a script, did the script name an image that exists. Each of those is a
//! row here, so the answer is a query rather than four log files.

use chrono::{DateTime, Utc};
use rainier_framework::prelude::*;

use crate::pxe::facts::ClientFacts;
use crate::pxe::policy::Decision;

/// Which conversation this row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A DHCP offer or acknowledgement.
    Offer,
    /// A file read over TFTP.
    Tftp,
    /// A file read over HTTP.
    Http,
    /// An iPXE script was served.
    Script,
    /// Something was refused.
    Refused,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Offer => "offer",
            EventKind::Tftp => "tftp",
            EventKind::Http => "http",
            EventKind::Script => "script",
            EventKind::Refused => "refused",
        }
    }
}

#[derive(Entity, Clone, Debug)]
#[orm(table = "boot_events")]
#[orm(index = "mac, at")]
#[orm(index = "at")]
pub struct BootEvent {
    #[orm(pk, auto_increment)]
    pub id: u64,

    pub mac: String,
    pub at: DateTime<Utc>,
    /// `offer`, `tftp`, `http`, `script`, `refused`.
    pub kind: String,
    pub arch: Option<String>,
    /// The profile the decision landed on, if this event had one.
    pub profile: Option<String>,
    /// `rule`, `pin`, `once`, `default`, `none` — where the profile came from.
    pub source: Option<String>,
    /// The first rule that fired.
    pub rule: Option<String>,
    /// The file handed over, or the reason for a refusal.
    pub detail: Option<String>,
    pub client_ip: Option<String>,
}

impl Model for BootEvent {}

impl BootEvent {
    /// A row for a decision this server made.
    pub fn decided(facts: &ClientFacts, kind: EventKind, decision: &Decision, detail: Option<String>) -> Self {
        Self {
            id: 0,
            mac: facts.mac.to_string(),
            at: facts.at,
            kind: kind.as_str().to_string(),
            arch: Some(facts.arch.label()),
            profile: decision.profile.clone(),
            source: Some(decision.source.as_str().to_string()),
            rule: decision.evaluation.matched.first().cloned(),
            detail,
            client_ip: facts.client_ip.map(|ip| ip.to_string()),
        }
    }

    /// A row for something that happened without a decision behind it — a file
    /// read, a refusal.
    pub fn plain(
        mac: String,
        kind: EventKind,
        detail: impl Into<String>,
        client_ip: Option<String>,
    ) -> Self {
        Self {
            id: 0,
            mac,
            at: Utc::now(),
            kind: kind.as_str().to_string(),
            arch: None,
            profile: None,
            source: None,
            rule: None,
            detail: Some(detail.into()),
            client_ip,
        }
    }

    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "mac": self.mac,
            "at": self.at,
            "kind": self.kind,
            "arch": self.arch,
            "profile": self.profile,
            "source": self.source,
            "rule": self.rule,
            "detail": self.detail,
            "client_ip": self.client_ip,
        })
    }

    /// One line, for `pxe:log` and the dashboard.
    pub fn summary(&self) -> String {
        let profile = self.profile.as_deref().unwrap_or("—");
        let detail = self.detail.as_deref().unwrap_or("");
        format!("{} {} {profile} {detail}", self.at.format("%Y-%m-%d %H:%M:%S"), self.kind)
            .trim_end()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::facts::Stage;
    use crate::pxe::policy::{decide, Overrides};
    use crate::pxe::rules::RuleSet;

    fn facts() -> ClientFacts {
        ClientFacts::new("18:66:da:11:22:33".parse().unwrap(), ClientArch::BIOS, Stage::Firmware)
    }

    #[test]
    fn a_decision_becomes_a_row_that_says_which_rule_chose_it() {
        // The whole point of the table: "why did this machine boot that".
        let rules = RuleSet::parse(
            r#"
[profiles.install]
kernel = "x"

[[rule]]
name = "everything-installs"
profile = "install"
"#,
        )
        .unwrap();

        let facts = facts();
        let decision = decide(&rules, &facts, &Overrides::default());
        let event = BootEvent::decided(&facts, EventKind::Offer, &decision, Some("ipxe.efi".into()));

        assert_eq!(event.mac, "18:66:da:11:22:33");
        assert_eq!(event.kind, "offer");
        assert_eq!(event.profile.as_deref(), Some("install"));
        assert_eq!(event.source.as_deref(), Some("rule"));
        assert_eq!(event.rule.as_deref(), Some("everything-installs"));
        assert_eq!(event.detail.as_deref(), Some("ipxe.efi"));
    }

    #[test]
    fn a_file_read_is_a_row_with_no_decision_behind_it() {
        let event = BootEvent::plain(
            "18:66:da:11:22:33".into(),
            EventKind::Tftp,
            "undionly.kpxe (94208 bytes)",
            Some("10.0.0.50".into()),
        );
        assert_eq!(event.kind, "tftp");
        assert_eq!(event.profile, None);
        assert!(event.summary().contains("undionly.kpxe"));
    }

    #[test]
    fn the_summary_does_not_trail_whitespace_when_there_is_no_detail() {
        let mut event = BootEvent::plain("aa:bb:cc:dd:ee:ff".into(), EventKind::Offer, "", None);
        event.detail = None;
        assert_eq!(event.summary(), event.summary().trim_end());
    }
}
