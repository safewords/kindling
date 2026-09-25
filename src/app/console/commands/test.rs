//! `pxe:test` — ask the policy what a machine would boot, without booting one.
//!
//! This is the command the whole rule engine is shaped around. A boot policy
//! is only debuggable if you can ask it a question and get the *reasoning*
//! back, not just the answer — so this prints every rule that was considered
//! and, for each one that did not fire, the first condition that failed.
//!
//! Nothing is written. A machine described here does not become a row, a
//! one-shot is not spent and no boot is counted.

use rainier_framework::console_kernel::{exit, io, Arguments, Command};
use rainier_framework::prelude::*;

use crate::app::services::BootService;
use crate::pxe::arch::{parse_arch, ClientArch};
use crate::pxe::facts::{ClientFacts, IpxeBuild, Stage};
use crate::pxe::mac::MacAddr;
use crate::pxe::policy;
use crate::pxe::rules::TraceOutcome;

#[derive(Debug, Default)]
pub struct TestCommand;

#[async_trait]
impl Command for TestCommand {
    fn name(&self) -> &str {
        "pxe:test"
    }

    fn description(&self) -> &str {
        "Ask what a machine would boot, and why"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:test --mac=18:66:da:11:22:33 [options]\n\n\
             Options:\n  \
             --mac=…            The machine's address (required)\n  \
             --arch=…           Architecture label or number (default x64-uefi)\n  \
             --stage=…          `firmware` or `ipxe` (default ipxe)\n  \
             --vendor-class=…   DHCP option 60\n  \
             --user-class=…     DHCP option 77 — `iPXE`, or `iPXE-kindling` for\n                     \
             this project's own build\n  \
             --hostname=…       DHCP option 12\n  \
             --product=…        SMBIOS product name, as iPXE reports it\n  \
             --manufacturer=…   SMBIOS manufacturer\n  \
             --serial=…         SMBIOS serial\n  \
             --asset=…          SMBIOS asset tag\n  \
             --ip=…             The address it is asking from\n  \
             --relay=…          The relay it came through, for subnet rules\n  \
             --script           Also print the iPXE script it would be sent\n  \
             --json             Emit the whole decision as JSON\n\n\
             Nothing is written: this does not create the machine in the inventory.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let service = app.resolve::<BootService>()?;

        let Some(raw_mac) = args.option("mac") else {
            eprintln!("pxe:test needs --mac=… : it is the one thing every rule can match on.");
            return Ok(exit::FAILURE);
        };

        let mac: MacAddr = match raw_mac.parse() {
            Ok(mac) => mac,
            Err(e) => {
                eprintln!("{e}");
                return Ok(exit::FAILURE);
            }
        };

        let arch = match args.option("arch") {
            Some(name) => match parse_arch(name) {
                Some(arch) => arch,
                None => {
                    eprintln!(
                        "`{name}` is not an architecture. Try one of:\n  {}",
                        ClientArch::known_labels().join(", ")
                    );
                    return Ok(exit::FAILURE);
                }
            },
            None => ClientArch::X64_UEFI,
        };

        let stage = match args.option("stage") {
            Some("firmware") | Some("dhcp") => Stage::Firmware,
            _ => Stage::Ipxe,
        };

        let facts = ClientFacts::new(mac, arch, stage)
            .with_vendor_class(args.option("vendor-class").map(str::to_string))
            .with_user_class(args.option("user-class").map(str::to_string))
            .with_hostname(args.option("hostname").map(str::to_string))
            .with_uuid(args.option("uuid").map(str::to_string))
            .with_smbios(
                args.option("manufacturer").map(str::to_string),
                args.option("product").map(str::to_string),
                args.option("serial").map(str::to_string),
                args.option("asset").map(str::to_string),
            )
            .with_client_ip(args.option("ip").and_then(|ip| ip.parse().ok()))
            .with_relay_ip(args.option("relay").and_then(|ip| ip.parse().ok()))
            .identified(service.ouis());

        let (facts, decision) = service.dry_run(facts).await;
        let rules = service.rules().current();

        if args.flag("json") {
            let script = policy::ipxe_script(&decision, &facts, &rules, service.settings())
                .unwrap_or_else(|e| format!("# {e}"));
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "facts": facts,
                    "profile": decision.profile,
                    "source": decision.source,
                    "reason": decision.reason,
                    "tags": decision.tags,
                    "trace": decision.evaluation.trace,
                    "script": script,
                }))
                .unwrap_or_default()
            );
            return Ok(exit::SUCCESS);
        }

        // `--user-class=iPXE-kindling` is how to ask about this project's own
        // build; it is still iPXE to every decision, and says so.
        let ipxe = match facts.ipxe_build() {
            IpxeBuild::NotIpxe => "false",
            IpxeBuild::Unidentified => "true",
            IpxeBuild::Own => "true (this project's build)",
        };
        println!("{}", facts.summary());
        println!(
            "  stage {}   device class {}   ipxe {ipxe}   known {}",
            facts.stage.as_str(),
            facts.device_class.as_str(),
            facts.known
        );
        if !facts.tags.is_empty() {
            println!("  tags  {}", facts.tags.join(", "));
        }

        println!("\nDecision");
        println!("  profile  {}", decision.profile.as_deref().unwrap_or("none"));
        println!("  from     {}", decision.source.as_str());
        println!("  because  {}", decision.reason);

        if let Some(answer) = policy::firmware_answer(&decision, &facts, &rules, service.settings())
        {
            println!(
                "\nAt the firmware stage this machine would be handed\n  {} ({})",
                answer.file, answer.reason
            );
        } else {
            println!("\nAt the firmware stage this server would not answer at all.");
        }

        if decision.evaluation.trace.is_empty() {
            println!("\nNo rules were considered — the policy has none.");
        } else {
            println!("\nEvery rule, in order");
            io::table(
                &["RULE", "OUTCOME"],
                &decision
                    .evaluation
                    .trace
                    .iter()
                    .map(|entry| vec![entry.rule.clone(), describe(&entry.outcome)])
                    .collect::<Vec<_>>(),
            );
        }

        if args.flag("script") {
            println!("\nThe script this machine would be served\n");
            match policy::ipxe_script(&decision, &facts, &rules, service.settings()) {
                Ok(script) => {
                    for line in script.lines() {
                        println!("  {line}");
                    }
                }
                Err(e) => println!("  (it could not be rendered: {e})"),
            }
        }

        Ok(exit::SUCCESS)
    }
}

/// What happened to one rule, in a sentence that fits in a table cell.
///
/// The `no match` case naming the field is the whole reason this command is
/// worth having: "did not match" sends somebody back to read the file, and
/// "no match (arch)" sends them to the line.
fn describe(outcome: &TraceOutcome) -> String {
    match outcome {
        TraceOutcome::Matched { profile, superseded, stopped } => {
            let mut text = match profile {
                Some(profile) => format!("matched → {profile}"),
                None => "matched (no profile)".to_string(),
            };
            if *superseded {
                text.push_str(", but an earlier rule had already chosen");
            }
            if *stopped {
                text.push_str(", stops here");
            }
            text
        }
        TraceOutcome::NoMatch { field, detail } if detail.is_empty() => format!("no match ({field})"),
        TraceOutcome::NoMatch { field, detail } => format!("no match ({field}: {detail})"),
        TraceOutcome::Excluded => "matched `when`, excluded by `unless`".to_string(),
        TraceOutcome::Disabled => "disabled".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outcome_of_a_rule_that_did_not_fire_names_the_condition() {
        assert_eq!(
            describe(&TraceOutcome::NoMatch {
                field: "arch".into(),
                detail: "arch is one of x64-uefi".into()
            }),
            "no match (arch: arch is one of x64-uefi)",
            "the field is the answer somebody is looking for"
        );
    }

    #[test]
    fn a_rule_that_fired_says_what_it_chose_and_whether_it_ended_evaluation() {
        assert_eq!(
            describe(&TraceOutcome::Matched {
                profile: Some("ubuntu".into()),
                superseded: false,
                stopped: true
            }),
            "matched → ubuntu, stops here"
        );

        assert_eq!(
            describe(&TraceOutcome::Matched { profile: None, superseded: false, stopped: false }),
            "matched (no profile)"
        );

        assert!(describe(&TraceOutcome::Matched {
            profile: Some("ubuntu".into()),
            superseded: true,
            stopped: false
        })
        .contains("already chosen"));
    }
}
