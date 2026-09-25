//! Turning facts into an answer.
//!
//! Three things decide what a machine boots, and they are tried in this order:
//!
//! 1. a **one-shot** set by an operator — "next boot only, install this";
//! 2. a **pin** — "this machine always boots that, ignore the rules";
//! 3. the **rule set**, and its default.
//!
//! One-shot beats pin because that is what "once" means: somebody standing at
//! a terminal overriding a standing decision for one boot. Pin beats the rules
//! because a pin is a human saying "I know, leave it alone", usually at the
//! point where the rules have got it wrong.
//!
//! Everything here is a pure function of its inputs. The database work — read
//! the overrides, write the boot event, clear the one-shot — happens around
//! it, in the application layer, which is what makes the interesting half
//! testable without one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::facts::{ClientFacts, Stage};
use super::profile::{self, Profile, ProfileKind, RenderContext, RenderError};
use super::rules::{Evaluation, RuleSet};

/// What the inventory knows about this machine, and what an operator has said
/// about it by hand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overrides {
    pub tags: Vec<String>,
    pub known: bool,
    pub boot_count: u64,
    /// Always this, until somebody says otherwise.
    pub pinned_profile: Option<String>,
    /// This, for the next boot only.
    pub once_profile: Option<String>,
}

/// Where the chosen profile came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionSource {
    /// A one-shot, which this boot consumes.
    Once,
    /// A pin.
    Pin,
    /// A rule in the rule file.
    Rule,
    /// `settings.default_profile`.
    Default,
    /// Nothing chose anything.
    None,
}

impl DecisionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionSource::Once => "once",
            DecisionSource::Pin => "pin",
            DecisionSource::Rule => "rule",
            DecisionSource::Default => "default",
            DecisionSource::None => "none",
        }
    }
}

/// The answer, before it is turned into a packet or a script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub profile: Option<String>,
    pub source: DecisionSource,
    /// Tags the rules attached, on top of the ones the machine already had.
    pub tags: Vec<String>,
    /// The full rule evaluation, including the rules that did not fire and
    /// why. This is what `pxe:test` prints and what the admin UI shows.
    pub evaluation: Evaluation,
    /// One sentence, for a log line.
    pub reason: String,
    /// Whether this boot should clear the machine's one-shot.
    pub consumes_once: bool,
}

impl Decision {
    /// Whether the server should answer at all.
    ///
    /// A profile of kind `ignore`, or no profile, means silence — which is a
    /// real answer in a network that has another boot server on it.
    pub fn answers(&self, rules: &RuleSet) -> bool {
        match &self.profile {
            None => false,
            Some(name) => rules.profile(name).map(Profile::kind) != Some(ProfileKind::Ignore),
        }
    }
}

/// Run the whole decision: overrides first, then the rules.
pub fn decide(rules: &RuleSet, facts: &ClientFacts, overrides: &Overrides) -> Decision {
    let evaluation = rules.evaluate(facts);

    // A pin or a one-shot naming a profile that no longer exists must not
    // silently become "boot nothing". The rules run regardless, and an
    // override pointing at a deleted profile falls through to them with the
    // reason saying so — the machine boots something sensible and the mistake
    // is visible rather than fatal.
    if let Some(once) = overrides.once_profile.as_deref() {
        if rules.profile(once).is_some() {
            return Decision {
                profile: Some(once.to_string()),
                source: DecisionSource::Once,
                tags: evaluation.tags.clone(),
                reason: format!("a one-shot boot of `{once}` was set for this machine"),
                consumes_once: facts.stage == Stage::Ipxe,
                evaluation,
            };
        }
    }

    if let Some(pin) = overrides.pinned_profile.as_deref() {
        if rules.profile(pin).is_some() {
            return Decision {
                profile: Some(pin.to_string()),
                source: DecisionSource::Pin,
                tags: evaluation.tags.clone(),
                reason: format!("this machine is pinned to `{pin}`"),
                consumes_once: false,
                evaluation,
            };
        }
    }

    let dangling = overrides
        .once_profile
        .as_deref()
        .or(overrides.pinned_profile.as_deref())
        .filter(|name| rules.profile(name).is_none());

    let source = match (&evaluation.profile, evaluation.used_default) {
        (None, _) => DecisionSource::None,
        (Some(_), true) => DecisionSource::Default,
        (Some(_), false) => DecisionSource::Rule,
    };

    let mut reason = evaluation.reason();
    if let Some(missing) = dangling {
        reason = format!(
            "`{missing}` is set for this machine but is not a profile any more, so the rules \
             decided instead: {reason}"
        );
    }

    Decision {
        profile: evaluation.profile.clone(),
        source,
        tags: evaluation.tags.clone(),
        reason,
        consumes_once: false,
        evaluation,
    }
}

/// How this server is reachable, which is most of what an answer contains.
#[derive(Debug, Clone)]
pub struct ServerSettings {
    /// The address machines should come back to. Written into `siaddr`, option
    /// 54 and every generated URL.
    pub server_ip: std::net::Ipv4Addr,
    /// The HTTP base, e.g. `http://10.0.0.2:8080`.
    pub http_base: String,
    /// What the PXE menu line says.
    pub description: String,
}

/// Which of the three answers this is.
///
/// Stated rather than inferred from the file name, because the loop breaker
/// has to tell "here is a loader to chainload" from "here is your script" —
/// and a machine that is handed the first when it wanted the second is
/// precisely the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerKind {
    /// The stage-one binary: iPXE, usually.
    ChainLoader,
    /// The iPXE script, for something that is already running iPXE.
    Script,
    /// A loader the profile named itself.
    ProfileBootFile,
}

/// What the firmware stage should be handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareAnswer {
    /// The boot file: a TFTP path, or an absolute URL for HTTP boot.
    pub file: String,
    /// Whether `file` is a URL the firmware will fetch over HTTP.
    pub over_http: bool,
    pub kind: AnswerKind,
    /// Why this file — for the boot log.
    pub reason: &'static str,
}

/// The iPXE binary for an architecture, when the rule file does not say.
///
/// These are the names the iPXE project's own builds carry, so an operator who
/// downloaded them and dropped them in the TFTP root has a server that works
/// with no `[bootloaders]` section at all.
pub fn default_bootloader(arch_label: &str) -> &'static str {
    match arch_label {
        "bios" | "bios-http" => "undionly.kpxe",
        "x86-uefi" | "x86-uefi-http" => "ipxe32.efi",
        "arm32-uefi" | "arm32-uefi-http" => "ipxe-arm32.efi",
        "arm64-uefi" | "arm64-uefi-http" => "ipxe-arm64.efi",
        _ => "ipxe.efi",
    }
}

/// Decide what to put in the boot file field of a DHCP answer.
///
/// Returns `None` when the server should stay quiet.
pub fn firmware_answer(
    decision: &Decision,
    facts: &ClientFacts,
    rules: &RuleSet,
    settings: &ServerSettings,
) -> Option<FirmwareAnswer> {
    if !decision.answers(rules) {
        return None;
    }

    let arch = facts.arch.label();

    // A profile may insist on its own loader — a machine that should be handed
    // `pxelinux.0` or a vendor's own binary and never chainloaded through
    // iPXE at all.
    if let Some(profile) = decision.profile.as_deref().and_then(|name| rules.profile(name)) {
        if let Some(file) = profile.bootfile_for(&arch) {
            return Some(FirmwareAnswer {
                over_http: is_url(file) || facts.is_http_boot(),
                file: absolute(file, settings, facts.is_http_boot()),
                kind: AnswerKind::ProfileBootFile,
                reason: "the profile names its own boot file",
            });
        }
    }

    // iPXE is already running: hand it its script rather than hand it itself.
    // Without this check the machine chainloads iPXE from iPXE, forever.
    if facts.is_ipxe() {
        return Some(script_answer(settings, "iPXE is asking, so it gets the script"));
    }

    let loader = rules.bootloader_for(&arch).unwrap_or_else(|| default_bootloader(&arch));
    Some(FirmwareAnswer {
        over_http: facts.is_http_boot(),
        file: absolute(loader, settings, facts.is_http_boot()),
        kind: AnswerKind::ChainLoader,
        reason: "chainloading iPXE",
    })
}

/// Hand the machine its script URL.
///
/// Split out because it is reached two ways: because iPXE was recognised, and
/// because the loop breaker worked out that it should have been.
pub fn script_answer(settings: &ServerSettings, reason: &'static str) -> FirmwareAnswer {
    FirmwareAnswer {
        file: format!("{}/boot.ipxe", settings.http_base.trim_end_matches('/')),
        over_http: true,
        kind: AnswerKind::Script,
        reason,
    }
}

fn is_url(file: &str) -> bool {
    file.starts_with("http://") || file.starts_with("https://") || file.starts_with("tftp://")
}

/// The HTTP path under which the TFTP root is also served.
///
/// One directory, two protocols: the machine that can only speak TFTP and the
/// machine that wants HTTP are fetching the same bytes, and an operator should
/// not have to put the loader in two places for that to be true.
pub const BOOT_PATH: &str = "boot";

/// HTTP-boot firmware needs an absolute URL; TFTP firmware needs a bare path.
/// Giving either one the other's form is a machine that does not boot.
fn absolute(file: &str, settings: &ServerSettings, http: bool) -> String {
    if is_url(file) {
        return file.to_string();
    }
    if http {
        format!(
            "{}/{BOOT_PATH}/{}",
            settings.http_base.trim_end_matches('/'),
            file.trim_start_matches('/')
        )
    } else {
        file.to_string()
    }
}

/// Render the iPXE script for a decision.
pub fn ipxe_script(
    decision: &Decision,
    facts: &ClientFacts,
    rules: &RuleSet,
    settings: &ServerSettings,
) -> Result<String, RenderError> {
    let Some(name) = decision.profile.as_deref() else {
        // Nothing matched and there is no default. Say so on the machine's own
        // console rather than dropping it at an iPXE prompt: somebody is
        // standing in front of it wondering.
        return Ok(format!(
            "#!ipxe\necho No boot policy matched {}\necho {}\nsleep 10\nexit 1\n",
            facts.mac,
            sanitise(&decision.reason)
        ));
    };

    let profile = rules
        .profile(name)
        .ok_or_else(|| RenderError::UnknownProfile(name.to_string()))?;

    let context = RenderContext {
        server: settings.server_ip.to_string(),
        base: settings.http_base.trim_end_matches('/').to_string(),
        facts,
        profiles: rules.profiles(),
        vars: &decision.evaluation.vars,
    };

    profile::render(name, profile, &context)
}

/// Render one named profile directly, for `/profiles/<name>.ipxe`.
pub fn render_named(
    name: &str,
    facts: &ClientFacts,
    rules: &RuleSet,
    settings: &ServerSettings,
) -> Result<String, RenderError> {
    let profile =
        rules.profile(name).ok_or_else(|| RenderError::UnknownProfile(name.to_string()))?;

    // A profile reached from a menu was not chosen by a rule, but the rules'
    // variables still describe this machine — so they are worked out again
    // here, and a `{{ var.role }}` in a menu entry means what it means
    // everywhere else.
    let vars = rules.evaluate(facts).vars;
    let context = RenderContext {
        server: settings.server_ip.to_string(),
        base: settings.http_base.trim_end_matches('/').to_string(),
        facts,
        profiles: rules.profiles(),
        vars: &vars,
    };

    profile::render(name, profile, &context)
}

/// Strip what iPXE's `echo` would choke on, so a reason with an awkward
/// character in it does not become a broken script.
fn sanitise(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\n' | '\r' | '$' | '|' | '&' | '#')).collect()
}

/// Every profile with its label, for a menu or an API listing.
pub fn profile_labels(rules: &RuleSet) -> BTreeMap<String, String> {
    rules.profiles().iter().map(|(name, profile)| (name.clone(), profile.label_or(name))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::oui::OuiDatabase;

    const RULES: &str = r#"
[settings]
default_profile = "menu"

[bootloaders]
bios = "ipxe/undionly.kpxe"
x64-uefi = "ipxe/ipxe.efi"

[profiles.menu]
kind = "menu"
entries = [{ profile = "local" }]

[profiles.local]
kind = "local"

[profiles.install]
label = "Ubuntu 24.04"
kernel = "{{base}}/ubuntu/vmlinuz"

[profiles.hands-off]
kind = "ignore"

[profiles.pxelinux]
kind = "local"
bootfile = { default = "pxelinux.0" }

[[rule]]
name = "vms-install"
when = { device_class = ["virtual"] }
profile = "install"

[[rule]]
name = "leave-the-switches-alone"
when = { device_class = ["network"] }
profile = "hands-off"
"#;

    fn rules() -> RuleSet {
        RuleSet::parse(RULES).unwrap()
    }

    fn settings() -> ServerSettings {
        ServerSettings {
            server_ip: "10.0.0.2".parse().unwrap(),
            http_base: "http://10.0.0.2:8080".into(),
            description: "kindling netboot".into(),
        }
    }

    fn facts(mac: &str, arch: ClientArch, stage: Stage) -> ClientFacts {
        ClientFacts::new(mac.parse().unwrap(), arch, stage).identified(&OuiDatabase::new())
    }

    #[test]
    fn a_one_shot_beats_a_pin_and_a_pin_beats_the_rules() {
        let rules = rules();
        let vm = facts("52:54:00:11:22:33", ClientArch::X64_UEFI, Stage::Ipxe);

        let by_rule = decide(&rules, &vm, &Overrides::default());
        assert_eq!(by_rule.profile.as_deref(), Some("install"));
        assert_eq!(by_rule.source, DecisionSource::Rule);

        let pinned = Overrides { pinned_profile: Some("local".into()), ..Default::default() };
        let by_pin = decide(&rules, &vm, &pinned);
        assert_eq!(by_pin.profile.as_deref(), Some("local"));
        assert_eq!(by_pin.source, DecisionSource::Pin);

        let both = Overrides {
            pinned_profile: Some("local".into()),
            once_profile: Some("menu".into()),
            ..Default::default()
        };
        let by_once = decide(&rules, &vm, &both);
        assert_eq!(by_once.profile.as_deref(), Some("menu"));
        assert_eq!(by_once.source, DecisionSource::Once);
    }

    #[test]
    fn a_one_shot_is_consumed_when_ipxe_takes_it_and_not_before() {
        // The DHCP stage sees the one-shot too, and clearing it there would
        // mean the machine never actually boots the thing.
        let rules = rules();
        let once = Overrides { once_profile: Some("install".into()), ..Default::default() };

        let firmware = decide(&rules, &facts("52:54:00:01:02:03", ClientArch::BIOS, Stage::Firmware), &once);
        assert!(!firmware.consumes_once);

        let ipxe = decide(&rules, &facts("52:54:00:01:02:03", ClientArch::BIOS, Stage::Ipxe), &once);
        assert!(ipxe.consumes_once);
    }

    #[test]
    fn an_override_naming_a_deleted_profile_falls_through_rather_than_bricking_the_boot() {
        // Somebody removed the profile and forgot the pin. The machine should
        // boot what the rules say, loudly.
        let rules = rules();
        let vm = facts("52:54:00:11:22:33", ClientArch::X64_UEFI, Stage::Ipxe);
        let stale = Overrides { pinned_profile: Some("deleted".into()), ..Default::default() };

        let decision = decide(&rules, &vm, &stale);
        assert_eq!(decision.profile.as_deref(), Some("install"));
        assert!(decision.reason.contains("not a profile any more"), "{}", decision.reason);
    }

    #[test]
    fn the_firmware_stage_is_handed_ipxe_and_the_ipxe_stage_is_handed_the_script() {
        // The check that stops the chainload loop.
        let rules = rules();
        let settings = settings();

        let firmware = facts("18:66:da:01:02:03", ClientArch::X64_UEFI, Stage::Firmware);
        let decision = decide(&rules, &firmware, &Overrides::default());
        let answer = firmware_answer(&decision, &firmware, &rules, &settings).unwrap();
        assert_eq!(answer.file, "ipxe/ipxe.efi");
        assert!(!answer.over_http);

        let ipxe = firmware.clone().with_user_class(Some("iPXE".into()));
        let answer = firmware_answer(&decision, &ipxe, &rules, &settings).unwrap();
        assert_eq!(answer.file, "http://10.0.0.2:8080/boot.ipxe");
        assert!(answer.over_http);
    }

    #[test]
    fn http_boot_firmware_is_given_a_url_and_tftp_firmware_a_path() {
        // Each one fails to boot on the other's form.
        let rules = rules();
        let settings = settings();

        let http = facts("18:66:da:01:02:03", ClientArch::X64_UEFI_HTTP, Stage::Firmware);
        let decision = decide(&rules, &http, &Overrides::default());
        let answer = firmware_answer(&decision, &http, &rules, &settings).unwrap();
        assert_eq!(
            answer.file, "http://10.0.0.2:8080/boot/ipxe.efi",
            "the same file the TFTP client gets, over the protocol this one speaks"
        );
        assert!(answer.over_http);
    }

    #[test]
    fn a_profile_can_insist_on_its_own_loader() {
        let rules = rules();
        let settings = settings();
        let machine = facts("18:66:da:01:02:03", ClientArch::BIOS, Stage::Firmware);
        let pinned = Overrides { pinned_profile: Some("pxelinux".into()), ..Default::default() };

        let decision = decide(&rules, &machine, &pinned);
        let answer = firmware_answer(&decision, &machine, &rules, &settings).unwrap();
        assert_eq!(answer.file, "pxelinux.0");
    }

    #[test]
    fn an_ignore_profile_means_the_server_stays_quiet() {
        // Which is a real answer on a network with another boot server on it.
        let rules = rules();
        let switch = facts("00:00:0c:01:02:03", ClientArch::BIOS, Stage::Firmware);
        let decision = decide(&rules, &switch, &Overrides::default());

        assert_eq!(decision.profile.as_deref(), Some("hands-off"));
        assert!(!decision.answers(&rules));
        assert_eq!(firmware_answer(&decision, &switch, &rules, &settings()), None);
    }

    #[test]
    fn the_script_for_a_machine_nothing_matched_says_so_on_its_console() {
        // Rather than dropping it at an iPXE prompt nobody is watching.
        let rules = RuleSet::parse("[profiles.x]\nkind = \"local\"\n").unwrap();
        let machine = facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS, Stage::Ipxe);
        let decision = decide(&rules, &machine, &Overrides::default());

        let script = ipxe_script(&decision, &machine, &rules, &settings()).unwrap();
        assert!(script.starts_with("#!ipxe\n"));
        assert!(script.contains("No boot policy matched aa:bb:cc:dd:ee:ff"), "{script}");
        assert!(script.contains("exit 1"), "{script}");
    }

    #[test]
    fn a_reason_with_awkward_characters_does_not_break_the_script() {
        let mut decision = decide(
            &RuleSet::default(),
            &facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS, Stage::Ipxe),
            &Overrides::default(),
        );
        decision.reason = "a ${var} | and\na newline".into();

        let script = ipxe_script(
            &decision,
            &facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS, Stage::Ipxe),
            &RuleSet::default(),
            &settings(),
        )
        .unwrap();

        assert_eq!(script.lines().count(), 5, "the reason stayed on one line: {script}");
        assert!(!script.contains("${var}"));
    }

    #[test]
    fn the_rendered_script_is_the_profile_the_decision_chose() {
        let rules = rules();
        let vm = facts("52:54:00:11:22:33", ClientArch::X64_UEFI, Stage::Ipxe);
        let decision = decide(&rules, &vm, &Overrides::default());

        let script = ipxe_script(&decision, &vm, &rules, &settings()).unwrap();
        assert!(script.contains("kernel http://10.0.0.2:8080/ubuntu/vmlinuz"), "{script}");
    }

    #[test]
    fn the_default_loaders_are_the_names_the_ipxe_project_ships() {
        assert_eq!(default_bootloader("bios"), "undionly.kpxe");
        assert_eq!(default_bootloader("x64-uefi"), "ipxe.efi");
        assert_eq!(default_bootloader("arm64-uefi"), "ipxe-arm64.efi");
        assert_eq!(default_bootloader("arch-9000"), "ipxe.efi", "an unknown arch gets the common one");
    }
}
