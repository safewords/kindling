//! Where policy meets the inventory.
//!
//! The rule engine is pure and the repositories are plain data access; this is
//! the piece that runs one against the other, and it is the only piece that
//! all three protocols share. A machine gets the same decision whether it
//! asked over DHCP, fetched a file over TFTP or came back to HTTP for a
//! script, because all three arrive here.
//!
//! One rule governs every method below: **a machine boots even when the
//! database does not**. The inventory is how this server explains itself
//! afterwards, not how it decides — so a failed query logs, degrades to "no
//! overrides, never seen before", and the rack comes up anyway.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use rainier_framework::prelude::*;

use crate::app::models::{BootEvent, EventKind, Host};
use crate::app::repositories::{BootEventRepository, HostRepository};
use crate::app::services::{LoopBreaker, LoopState, RuleStore};
use crate::pxe::dhcp::proxy::BootPolicy;
use crate::pxe::facts::ClientFacts;
use crate::pxe::mac::MacAddr;
use crate::pxe::oui::OuiDatabase;
use crate::pxe::policy::{
    self, AnswerKind, Decision, FirmwareAnswer, Overrides, ServerSettings,
};
use crate::pxe::profile::RenderError;
use crate::pxe::tftp::server::{ReadOutcome, TftpEvents};

pub struct BootService {
    rules: Arc<RuleStore>,
    ouis: Arc<OuiDatabase>,
    settings: ServerSettings,
    hosts: Arc<HostRepository>,
    events: Arc<BootEventRepository>,
    loops: Arc<LoopBreaker>,
}

impl BootService {
    pub fn new(
        rules: Arc<RuleStore>,
        ouis: Arc<OuiDatabase>,
        settings: ServerSettings,
        hosts: Arc<HostRepository>,
        events: Arc<BootEventRepository>,
        loops: Arc<LoopBreaker>,
    ) -> Self {
        Self { rules, ouis, settings, hosts, events, loops }
    }

    pub fn loops(&self) -> &Arc<LoopBreaker> {
        &self.loops
    }

    pub fn rules(&self) -> &Arc<RuleStore> {
        &self.rules
    }

    pub fn ouis(&self) -> &Arc<OuiDatabase> {
        &self.ouis
    }

    pub fn settings(&self) -> &ServerSettings {
        &self.settings
    }

    /// Look a machine up, decide what it boots, and record the sighting.
    ///
    /// Returns the facts *as enriched by the inventory* alongside the
    /// decision, because the caller renders a script from them and a rule that
    /// matched on a tag should render with that tag in scope.
    pub async fn decide(&self, facts: ClientFacts) -> (ClientFacts, Decision) {
        let rules = self.rules.current();

        let (overrides, host) = match self.hosts.observe(&facts).await {
            // `known` comes off the row's boot count rather than from whether
            // this call created the row. One boot is three or four requests —
            // a DHCP offer, a file fetch, then the script — and only the last
            // of them picks an image. Answering "is there a row" would make a
            // machine known by its second request, so `known = false` would
            // fire at the DHCP stage, where nothing is chosen, and never at
            // the stage where something is.
            Ok((host, _)) => (host.overrides(), Some(host)),
            Err(e) => {
                // The rack comes up anyway. An inventory this server cannot
                // reach is a reporting problem; a machine that will not boot
                // is an outage.
                tracing::error!(
                    mac = %facts.mac,
                    error = %e,
                    "could not reach the inventory; deciding from the rules alone"
                );
                (Overrides::default(), None)
            }
        };

        let facts = facts.with_inventory(
            overrides.tags.clone(),
            overrides.known,
            overrides.boot_count,
        );

        let decision = policy::decide(&rules, &facts, &overrides);

        if let Some(mut host) = host {
            let mut changed = host.apply_tags(&decision.tags);
            for tag in &decision.evaluation.removed_tags {
                changed |= host.remove_tag(tag);
            }
            if changed {
                if let Err(e) = self.hosts.save(&host).await {
                    tracing::warn!(mac = %facts.mac, error = %e, "could not save the rule's tags");
                }
            }
        }

        (facts, decision)
    }

    /// Facts for a machine, enriched from the inventory but changing nothing.
    ///
    /// This is what `pxe:test` and the rule tester in the admin UI use: a
    /// dry run must not create a host row for a machine that does not exist.
    pub async fn dry_run(&self, facts: ClientFacts) -> (ClientFacts, Decision) {
        let rules = self.rules.current();
        let host = self.hosts.by_mac(facts.mac).await.ok().flatten();

        let overrides = host.as_ref().map(Host::overrides).unwrap_or_default();
        let facts =
            facts.with_inventory(overrides.tags.clone(), overrides.known, overrides.boot_count);
        let decision = policy::decide(&rules, &facts, &overrides);

        (facts, decision)
    }

    /// The iPXE script for a machine, and the bookkeeping that goes with
    /// handing one over.
    ///
    /// Serving the script is the moment a machine commits to a profile, so
    /// this is where the boot counter moves and a one-shot is spent — not at
    /// the DHCP stage, which happens whether or not anything gets that far.
    pub async fn script(&self, facts: ClientFacts) -> Result<String> {
        let (facts, decision) = self.decide(facts).await;
        let rules = self.rules.current();

        let script = policy::ipxe_script(&decision, &facts, &rules, &self.settings)
            .map_err(render_error)?;

        // Getting this far is proof the machine is not going in circles,
        // whatever the DHCP stage suspected.
        self.loops.progressed(facts.mac);

        if decision.consumes_once {
            if let Some(profile) = decision.profile.as_deref() {
                match self.hosts.consume_once(facts.mac, profile).await {
                    Ok(true) => tracing::info!(
                        mac = %facts.mac,
                        profile,
                        "one-shot boot used up; the next boot follows the rules again"
                    ),
                    Ok(false) => tracing::debug!(
                        mac = %facts.mac,
                        "the one-shot changed between the decision and the clear; left alone"
                    ),
                    Err(e) => tracing::warn!(
                        mac = %facts.mac,
                        error = %e,
                        "could not clear the one-shot, so this machine may boot it again"
                    ),
                }
            }
        }

        if let Err(e) =
            self.hosts.record_boot(facts.mac, decision.profile.as_deref(), Utc::now()).await
        {
            tracing::warn!(mac = %facts.mac, error = %e, "could not record the boot");
        }

        self.record(BootEvent::decided(
            &facts,
            EventKind::Script,
            &decision,
            Some(format!("served a script ({})", decision.source.as_str())),
        ))
        .await;

        tracing::info!(
            mac = %facts.mac,
            profile = decision.profile.as_deref().unwrap_or("none"),
            source = decision.source.as_str(),
            reason = decision.reason,
            "served an iPXE script"
        );

        Ok(script)
    }

    /// One named profile, rendered for a machine. Nothing is recorded: this is
    /// a menu selection being followed, and the boot it leads to is what gets
    /// logged.
    pub async fn named_profile(&self, name: &str, facts: ClientFacts) -> Result<String> {
        let rules = self.rules.current();
        policy::render_named(name, &facts, &rules, &self.settings).map_err(render_error)
    }

    /// Record something that happened without a decision behind it.
    pub async fn note(&self, event: BootEvent) {
        self.record(event).await;
    }

    async fn record(&self, event: BootEvent) {
        if let Err(e) = self.events.record(event).await {
            // Deliberately not propagated. A log row that could not be written
            // is not a reason to fail a boot.
            tracing::warn!(error = %e, "could not write a boot event");
        }
    }
}

impl BootService {
    /// Hand a machine its script if it is plainly going round in circles.
    ///
    /// Reached only when everything else has failed to recognise iPXE — the
    /// user class *and* option 175. When a machine has completed several whole
    /// DHCP transactions for the same loader inside the window, it is running
    /// that loader and coming back, and the script is what it was trying to
    /// reach. Switching to it turns a hang into a boot.
    ///
    /// The log line matters as much as the recovery. A loop that is named is a
    /// five-minute problem; one that is silently worked around is a mystery
    /// that comes back on the next fleet.
    fn break_any_loop(&self, facts: &ClientFacts, answer: FirmwareAnswer) -> FirmwareAnswer {
        // Only a chainload can loop. A profile's own boot file is a deliberate
        // choice, and the script is where a loop ends.
        if answer.kind != AnswerKind::ChainLoader {
            return answer;
        }
        // No transaction id means this did not come from a DHCP packet, and
        // the whole discriminator is the transaction id.
        let Some(xid) = facts.transaction else { return answer };

        match self.loops.offer(facts.mac, &answer.file, xid, std::time::Instant::now()) {
            LoopState::Fine => answer,
            LoopState::Looping { transactions } => {
                tracing::error!(
                    mac = %facts.mac,
                    file = answer.file,
                    transactions,
                    arch = %facts.arch,
                    vendor_class = facts.vendor_class.as_deref().unwrap_or(""),
                    user_class = facts.user_class.as_deref().unwrap_or(""),
                    ipxe_options = facts.ipxe_options,
                    "this machine has been offered the same boot loader across {transactions} \
                     whole DHCP transactions without reaching a script, so it is chainloading in \
                     a circle. Serving the script instead. It was not recognised as iPXE: check \
                     that option 77 or option 175 survives the path from this machine — a relay \
                     that strips them is the usual cause."
                );

                policy::script_answer(&self.settings, "the chainload was looping")
            }
        }
    }
}

fn render_error(e: RenderError) -> Error {
    Error::internal(format!("the boot script could not be rendered: {e}"))
}

/// The DHCP server's view of this service.
#[async_trait]
impl BootPolicy for BootService {
    async fn firmware_answer(&self, facts: ClientFacts) -> Option<FirmwareAnswer> {
        let (facts, decision) = self.decide(facts).await;
        let rules = self.rules.current();

        let answer = policy::firmware_answer(&decision, &facts, &rules, &self.settings)
            .map(|answer| self.break_any_loop(&facts, answer));

        match &answer {
            Some(answer) => {
                tracing::info!(
                    mac = %facts.mac,
                    arch = %facts.arch,
                    vendor = facts.vendor.as_deref().unwrap_or("unknown"),
                    file = answer.file,
                    profile = decision.profile.as_deref().unwrap_or("none"),
                    reason = answer.reason,
                    "offering a boot file"
                );
                self.record(BootEvent::decided(
                    &facts,
                    EventKind::Offer,
                    &decision,
                    // The reason rides along, so a broken loop shows up in
                    // `pxe:log` and on the dashboard rather than only in the
                    // process log.
                    Some(format!("{} ({})", answer.file, answer.reason)),
                ))
                .await;
            }
            None => {
                self.record(BootEvent::decided(
                    &facts,
                    EventKind::Refused,
                    &decision,
                    Some(decision.reason.clone()),
                ))
                .await;
            }
        }

        answer
    }
}

/// The TFTP server's view: every read is a line in the boot log.
///
/// A file read is the only evidence that an offer was acted on. "The machine
/// got an offer and never fetched the loader" is a different problem from "it
/// fetched the loader and nothing happened", and without this row the two look
/// identical.
#[async_trait]
impl TftpEvents for BootService {
    async fn read(&self, filename: String, peer: SocketAddr, outcome: ReadOutcome) {
        let (kind, detail) = match &outcome {
            ReadOutcome::Served { bytes } => {
                (EventKind::Tftp, format!("{filename} ({bytes} bytes)"))
            }
            ReadOutcome::NotFound => (EventKind::Refused, format!("{filename}: not found")),
            ReadOutcome::Denied(why) => (EventKind::Refused, format!("{filename}: {why}")),
            ReadOutcome::Failed(why) => (EventKind::Refused, format!("{filename}: {why}")),
        };

        // TFTP carries no hardware address — the protocol has no field for
        // one. The machine is identified by the address it is reading from,
        // which is the address this server offered a file to moments earlier.
        let mac = self
            .mac_for_address(&peer)
            .await
            .map(|mac| mac.to_string())
            .unwrap_or_else(|| format!("ip:{}", peer.ip()));

        self.record(BootEvent::plain(mac, kind, detail, Some(peer.ip().to_string()))).await;
    }
}

impl BootService {
    /// Which machine is at this address, as far as the inventory knows.
    ///
    /// Best effort by design: a machine whose address changed since its last
    /// boot is logged by address instead, which is still more useful than
    /// nothing and is never *wrong* about which machine did what.
    async fn mac_for_address(&self, peer: &SocketAddr) -> Option<MacAddr> {
        let ip = peer.ip().to_string();
        let hosts = self.hosts.matching(Criteria::new().where_eq("last_ip", ip).limit(2)).await.ok()?;

        // Two machines have claimed this address since the server started.
        // Guessing between them would put the wrong machine in the log, so
        // neither is chosen.
        match hosts.as_slice() {
            [host] => host.address(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::facts::Stage;
    use crate::pxe::rules::RuleSet;

    const RULES: &str = r#"
[settings]
default_profile = "menu"

[profiles.menu]
kind = "menu"
entries = [{ profile = "local" }]

[profiles.local]
kind = "local"

[profiles.install]
kernel = "{{base}}/vmlinuz"

[[rule]]
name = "new-machines-install"
when = { known = false }
profile = "install"
tag = ["provisioned"]
"#;

    #[test]
    fn a_rule_on_known_reads_the_boot_that_created_the_row_as_new() {
        // The subtle one. `observe` creates the row before the decision is
        // made, so reading `known` off the row would make every machine known
        // on its very first boot and the rule would never fire once.
        let rules = RuleSet::parse(RULES).unwrap();
        let facts = ClientFacts::new(
            "18:66:da:11:22:33".parse().unwrap(),
            ClientArch::X64_UEFI,
            Stage::Ipxe,
        );

        // `with_inventory` is the step `decide` performs before evaluating,
        // and it is what carries `known` into the facts a rule matches on.
        let first = policy::decide(
            &rules,
            &facts.clone().with_inventory(Vec::new(), false, 0),
            &Overrides { known: false, ..Default::default() },
        );
        assert_eq!(first.profile.as_deref(), Some("install"));

        let second = policy::decide(
            &rules,
            &facts.with_inventory(Vec::new(), true, 1),
            &Overrides { known: true, boot_count: 1, ..Default::default() },
        );
        assert_eq!(second.profile.as_deref(), Some("menu"), "the second boot is not a new machine");
    }

    #[test]
    fn an_unreachable_inventory_still_produces_a_decision() {
        // The degraded path, asserted on directly: with no overrides at all, a
        // machine still gets whatever the rules say.
        let rules = RuleSet::parse(RULES).unwrap();
        let facts = ClientFacts::new(
            "18:66:da:11:22:33".parse().unwrap(),
            ClientArch::X64_UEFI,
            Stage::Ipxe,
        );

        let decision = policy::decide(&rules, &facts, &Overrides::default());
        assert!(decision.profile.is_some(), "a database outage is not a reason not to boot");
    }
}
