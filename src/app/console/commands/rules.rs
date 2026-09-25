//! `pxe:rules` — read, check, import, export and roll back the boot policy.
//!
//! The policy lives in the database and is normally edited in the web UI.
//! This command is for everything around that: checking a file in a deploy
//! pipeline before importing it, importing it, exporting the running policy
//! for review or backup, and rolling back from a terminal when the web UI is
//! the thing that is broken.

use rainier_framework::console_kernel::{exit, io, Arguments, Command};
use rainier_framework::prelude::*;

use crate::app::services::{RuleStore, STARTER_POLICY};
use crate::pxe::rules::RuleSet;

#[derive(Debug, Default)]
pub struct RulesCommand;

#[async_trait]
impl Command for RulesCommand {
    fn name(&self) -> &str {
        "pxe:rules"
    }

    fn description(&self) -> &str {
        "Show, check, import, export or roll back the boot policy"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:rules                    Show the policy in force\n  \
             pxe:rules --check=FILE       Validate a TOML or JSON policy file, changing nothing\n  \
             pxe:rules --import=FILE      Replace the stored policy with a file's (kept as a revision)\n  \
             pxe:rules --export[=toml]    Print the running policy as JSON, or TOML\n  \
             pxe:rules --history          List the stored revisions, newest first\n  \
             pxe:rules --restore=ID       Put revision ID back (itself a new revision)\n  \
             pxe:rules --reload           Re-read the stored policy into the running server\n  \
             pxe:rules --example          Print the starter policy\n\n\
             `--check` is what a deploy pipeline runs: it exits non-zero and lists every\n\
             problem at once, so a file with five mistakes is one fix rather than five.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        if args.flag("example") {
            println!("{STARTER_POLICY}");
            return Ok(exit::SUCCESS);
        }

        if let Some(path) = args.option("check") {
            return Ok(check(path));
        }

        let store = app.resolve::<RuleStore>()?;

        if let Some(path) = args.option("import") {
            return Ok(match store.import(std::path::Path::new(path), "console").await {
                Ok(applied) => {
                    println!(
                        "Imported `{path}`: {} rule(s), {} profile(s){}.",
                        applied.rules.rules().len(),
                        applied.rules.profiles().len(),
                        applied
                            .revision
                            .map(|r| format!(", revision {}", r.id))
                            .unwrap_or_else(|| " — identical to what was stored".into())
                    );
                    exit::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}\n\nNothing changed; the stored policy is as it was.");
                    exit::FAILURE
                }
            });
        }

        if args.flag("export") || args.option("export").is_some() {
            let document = store.current().to_document();
            match args.option("export") {
                Some("toml") => match document.to_toml() {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("the policy could not be written as TOML: {e}");
                        return Ok(exit::FAILURE);
                    }
                },
                _ => println!("{}", serde_json::to_string_pretty(&document).unwrap_or_default()),
            }
            return Ok(exit::SUCCESS);
        }

        if args.flag("history") {
            let Some(repository) = store.repository() else {
                eprintln!("this policy has no database behind it, so no history");
                return Ok(exit::FAILURE);
            };
            let current = store.revision();
            io::table(
                &["REVISION", "WHEN", "BY", "RULES", "PROFILES", "CHANGE"],
                &repository
                    .revisions(50)
                    .await?
                    .iter()
                    .map(|r| {
                        vec![
                            if Some(r.id) == current { format!("{} *", r.id) } else { r.id.to_string() },
                            r.created_at.format("%Y-%m-%d %H:%M").to_string(),
                            r.actor.clone(),
                            r.rules.to_string(),
                            r.profiles.to_string(),
                            r.summary.clone(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            return Ok(exit::SUCCESS);
        }

        if let Some(raw) = args.option("restore") {
            let Ok(id) = raw.parse::<u64>() else {
                eprintln!("`{raw}` is not a revision number; `--history` lists them");
                return Ok(exit::FAILURE);
            };
            return Ok(match store.restore(id, "console").await {
                Ok(applied) => {
                    println!(
                        "Restored revision {id}: {} rule(s), {} profile(s).",
                        applied.rules.rules().len(),
                        applied.rules.profiles().len()
                    );
                    exit::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}\n\nNothing changed.");
                    exit::FAILURE
                }
            });
        }

        if args.flag("reload") {
            return match store.reload().await {
                Ok(rules) => {
                    println!(
                        "Reloaded: {} rule(s), {} profile(s).",
                        rules.rules().len(),
                        rules.profiles().len()
                    );
                    Ok(exit::SUCCESS)
                }
                Err(e) => {
                    eprintln!("{e}");
                    eprintln!("\nNothing changed; the policy already running is still in force.");
                    Ok(exit::FAILURE)
                }
            };
        }

        show(&store);
        Ok(exit::SUCCESS)
    }
}

fn check(path: &str) -> i32 {
    match RuleSet::load(path) {
        Ok(rules) => {
            println!(
                "{path}: {} rule(s), {} profile(s). No problems.",
                rules.rules().len(),
                rules.profiles().len()
            );

            // Not a failure — a policy can legitimately be all tagging rules —
            // but it is the single most common reason for "nothing boots", so
            // it is said out loud.
            if rules.settings().default_profile.is_none()
                && !rules.rules().iter().any(|rule| rule.profile.is_some())
            {
                println!(
                    "\nNote: nothing in this policy chooses a profile, so no machine would be \
                     told to boot anything."
                );
            }
            exit::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            exit::FAILURE
        }
    }
}

fn show(store: &RuleStore) {
    let rules = store.current();

    println!(
        "Policy from the {}{}  —  loaded {}",
        rules.source(),
        store.revision().map(|r| format!(", revision {r}")).unwrap_or_default(),
        rules.loaded_at().format("%Y-%m-%d %H:%M:%S UTC")
    );

    if let Some((at, message)) = store.last_error() {
        println!(
            "\n! The stored policy does not load, so what is running is older than it.\n  \
             Last tried {}: {message}",
            at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    println!(
        "\nDefault profile: {}",
        rules.settings().default_profile.as_deref().unwrap_or("none")
    );

    if !rules.bootloaders().is_empty() {
        println!("\nBoot loaders");
        io::table(
            &["ARCHITECTURE", "FILE"],
            &rules
                .bootloaders()
                .iter()
                .map(|(arch, file)| vec![arch.clone(), file.clone()])
                .collect::<Vec<_>>(),
        );
    }

    println!("\nProfiles");
    io::table(
        &["NAME", "KIND", "LABEL"],
        &rules
            .profiles()
            .iter()
            .map(|(name, profile)| {
                vec![
                    name.clone(),
                    format!("{:?}", profile.kind()).to_lowercase(),
                    profile.label_or(name),
                ]
            })
            .collect::<Vec<_>>(),
    );

    println!("\nRules, in the order they are evaluated");
    io::table(
        &["PRIORITY", "NAME", "DOES", "STOPS", "WHEN"],
        &rules
            .rules()
            .iter()
            .map(|rule| {
                let (when, unless) = rules.describe(&rule.name).unwrap_or_default();
                vec![
                    rule.priority.to_string(),
                    if rule.enabled { rule.name.clone() } else { format!("{} (off)", rule.name) },
                    actions(rule),
                    if rule.stops() { "yes".into() } else { "no".into() },
                    match unless {
                        Some(unless) => format!("{when}, unless {unless}"),
                        None => when,
                    },
                ]
            })
            .collect::<Vec<_>>(),
    );
}

/// What a rule does, in a table cell.
fn actions(rule: &crate::pxe::rules::Rule) -> String {
    let mut does = Vec::new();
    if let Some(profile) = &rule.profile {
        does.push(format!("boot {profile}"));
    }
    if !rule.tag.is_empty() {
        does.push(format!("+{}", rule.tag.join(" +")));
    }
    if !rule.remove_tags.is_empty() {
        does.push(format!("-{}", rule.remove_tags.join(" -")));
    }
    for (key, value) in &rule.set {
        does.push(format!("{key}={value}"));
    }
    if does.is_empty() {
        "—".into()
    } else {
        does.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_this_command_prints_is_a_policy_that_loads() {
        // It is also what a new install is seeded with, so this asserts the
        // default is valid as well.
        let rules = RuleSet::parse(STARTER_POLICY).expect("the starter policy must be valid");
        assert!(!rules.rules().is_empty(), "an example with no rules teaches nothing");
        assert!(rules.settings().default_profile.is_some(), "and it should boot something");
    }

    #[test]
    fn the_summary_says_what_a_rule_does() {
        let rules = RuleSet::parse(
            r#"
[profiles.a]
kind = "local"

[[rule]]
name = "r"
profile = "a"
tag = ["x"]
remove_tags = ["y"]
set = { role = "db" }
when = { vendor = ["Dell"], arch = ["x64-uefi"], known = false }
"#,
        )
        .unwrap();

        assert_eq!(actions(&rules.rules()[0]), "boot a, +x, -y, role=db");
        let (when, _) = rules.describe("r").unwrap();
        assert!(when.contains("vendor") && when.contains("arch") && when.contains("known"), "{when}");
    }

    #[test]
    fn a_rule_with_no_conditions_says_so() {
        let rules = RuleSet::parse("[profiles.a]\nkind = \"local\"\n\n[[rule]]\nname = \"r\"\nprofile = \"a\"\n").unwrap();
        assert_eq!(rules.describe("r").unwrap().0, "anything");
    }
}
