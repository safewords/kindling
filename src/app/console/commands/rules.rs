//! `pxe:rules` — read the boot policy, its history, and roll it back.
//!
//! The policy lives in the database. It is changed in the web UI, through the
//! API, or with `pxe:rule`, `pxe:profile` and `pxe:policy`; this command shows
//! what is in force and moves between revisions. There is deliberately no
//! import or export: a policy kept in a file is a second copy that drifts, and
//! the database's revisions are the history and the backup (`pxe:db:backup`
//! copies the whole database).

use rainier_framework::console_kernel::{exit, io, Arguments, Command};
use rainier_framework::prelude::*;

use crate::app::services::RuleStore;

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
             pxe:rules --history          List the stored revisions, newest first\n  \
             pxe:rules --restore=ID       Put revision ID back (itself a new revision)\n  \
             pxe:rules --reload           Re-read the stored policy into this process\n\n\
             To change the policy: pxe:rule, pxe:profile, pxe:policy (or the web UI).\n\
             A running pxe:serve picks a change up within a few seconds.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        for refused in ["import", "export", "check", "example"] {
            if args.flag(refused) || args.option(refused).is_some() {
                eprintln!(
                    "`--{refused}` is gone: the boot policy is not kept in files. It lives in \
                     the database; change it with pxe:rule, pxe:profile and pxe:policy, roll it \
                     back with --restore, and back it up with pxe:db:backup."
                );
                return Ok(exit::FAILURE);
            }
        }

        let store = app.resolve::<RuleStore>()?;

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
                            if Some(r.id) == current {
                                format!("{} *", r.id)
                            } else {
                                r.id.to_string()
                            },
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

fn show(store: &RuleStore) {
    let rules = store.current();

    println!(
        "Policy from the {}{}  —  loaded {}",
        rules.source(),
        store
            .revision()
            .map(|r| format!(", revision {r}"))
            .unwrap_or_default(),
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
        rules
            .settings()
            .default_profile
            .as_deref()
            .unwrap_or("none")
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
                    if rule.enabled {
                        rule.name.clone()
                    } else {
                        format!("{} (off)", rule.name)
                    },
                    describe_actions(rule),
                    if rule.stops() {
                        "yes".into()
                    } else {
                        "no".into()
                    },
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
pub fn describe_actions(rule: &crate::pxe::rules::Rule) -> String {
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
    fn the_summary_says_what_a_rule_does() {
        let rules = crate::pxe::rules::RuleSet::from_json_value(serde_json::json!({
            "profiles": {
                "a": {
                    "kind": "local"
                }
            },
            "rule": [
                {
                    "name": "r",
                    "profile": "a",
                    "tag": [
                        "x"
                    ],
                    "remove_tags": [
                        "y"
                    ],
                    "set": {
                        "role": "db"
                    },
                    "when": {
                        "vendor": [
                            "Dell"
                        ],
                        "arch": [
                            "x64-uefi"
                        ],
                        "known": false
                    }
                }
            ]
        }))
        .unwrap();

        assert_eq!(
            describe_actions(&rules.rules()[0]),
            "boot a, +x, -y, role=db"
        );
        let (when, _) = rules.describe("r").unwrap();
        assert!(
            when.contains("vendor") && when.contains("arch") && when.contains("known"),
            "{when}"
        );
    }

    #[test]
    fn a_rule_with_no_conditions_says_so() {
        let rules = crate::pxe::rules::RuleSet::from_json_value(serde_json::json!({
            "profiles": {
                "a": {
                    "kind": "local"
                }
            },
            "rule": [
                {
                    "name": "r",
                    "profile": "a"
                }
            ]
        }))
        .unwrap();
        assert_eq!(rules.describe("r").unwrap().0, "anything");
    }
}
