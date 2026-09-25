//! The commands an operator runs at a terminal with a machine in front of
//! them: what is out there, what did it do, and make this one boot that.

use rainier_framework::console_kernel::{exit, io, Arguments, Command};
use rainier_framework::prelude::*;

use crate::app::repositories::{BootEventRepository, HostRepository};
use crate::app::services::RuleStore;
use crate::pxe::mac::MacAddr;

/// `pxe:hosts` — the inventory.
#[derive(Debug, Default)]
pub struct HostsCommand;

#[async_trait]
impl Command for HostsCommand {
    fn name(&self) -> &str {
        "pxe:hosts"
    }

    fn description(&self) -> &str {
        "List the machines this server has seen"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:hosts [--search=TEXT] [--tag=TAG] [--limit=N] [--json]\n\n\
             `--search` looks in the address, hostname, vendor, product and serial —\n\
             whatever is written on the sticker you are holding.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let hosts = app.resolve::<HostRepository>()?;
        let limit: u64 = args.parsed_or("limit", 100u64).clamp(1, 1000);

        let found =
            hosts.page(1, limit, args.option("search"), args.option("tag")).await?;

        if args.flag("json") {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "data": found.data.iter().map(crate::app::models::Host::as_json).collect::<Vec<_>>(),
                    "total": found.total,
                }))
                .unwrap_or_default()
            );
            return Ok(exit::SUCCESS);
        }

        if found.data.is_empty() {
            println!("No machines yet. They appear here the first time they ask to boot.");
            return Ok(exit::SUCCESS);
        }

        io::table(
            &["MAC", "VENDOR", "ARCH", "HOSTNAME", "LAST PROFILE", "PIN", "ONCE", "BOOTS", "LAST SEEN", "TAGS"],
            &found
                .data
                .iter()
                .map(|host| {
                    vec![
                        host.mac.clone(),
                        host.vendor.clone().unwrap_or_else(|| "—".into()),
                        host.arch.clone(),
                        host.hostname.clone().or_else(|| host.product.clone()).unwrap_or_else(|| "—".into()),
                        host.last_profile.clone().unwrap_or_else(|| "—".into()),
                        host.pinned_profile.clone().unwrap_or_else(|| "—".into()),
                        host.once_profile.clone().unwrap_or_else(|| "—".into()),
                        host.boot_count.to_string(),
                        host.last_seen.format("%Y-%m-%d %H:%M").to_string(),
                        if host.tags.0.is_empty() { "—".into() } else { host.tags.0.join(", ") },
                    ]
                })
                .collect::<Vec<_>>(),
        );

        println!("{} of {} machine(s)", found.data.len(), found.total);
        Ok(exit::SUCCESS)
    }
}

/// `pxe:pin` — make one machine boot one thing.
#[derive(Debug, Default)]
pub struct PinCommand;

#[async_trait]
impl Command for PinCommand {
    fn name(&self) -> &str {
        "pxe:pin"
    }

    fn description(&self) -> &str {
        "Pin a machine to a profile, or set a one-shot boot"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:pin --mac=… --profile=NAME     Always boot NAME, ignoring the rules\n  \
             pxe:pin --mac=… --profile=NAME --once\n                                    \
             Boot NAME on the next boot only\n  \
             pxe:pin --mac=… --clear            Remove the pin\n  \
             pxe:pin --mac=… --clear --once     Remove the one-shot\n\n\
             A one-shot beats a pin, and both beat the rules. The one-shot is spent\n\
             when the machine is actually served its script, not when it is offered\n\
             a boot file — so a machine that is offered one and never comes back\n\
             still has its one-shot waiting.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let Some(mac) = mac_argument(args) else { return Ok(exit::FAILURE) };
        let hosts = app.resolve::<HostRepository>()?;
        let once = args.flag("once");

        if args.flag("clear") {
            let cleared = match once {
                true => hosts.set_once(mac, None).await?,
                false => hosts.pin(mac, None).await?,
            };
            if !cleared {
                eprintln!("No machine with the address {mac} has been seen.");
                return Ok(exit::FAILURE);
            }
            println!("{mac}: {} cleared.", if once { "one-shot" } else { "pin" });
            return Ok(exit::SUCCESS);
        }

        let Some(profile) = args.option("profile") else {
            eprintln!("pxe:pin needs --profile=NAME, or --clear.");
            return Ok(exit::FAILURE);
        };

        // Checked before it is stored. A pin at a profile that does not exist
        // falls through to the rules, so the operator would watch the machine
        // boot something else with no idea why.
        let rules = app.resolve::<RuleStore>()?.current();
        if rules.profile(profile).is_none() {
            eprintln!(
                "`{profile}` is not a profile. Declared:\n  {}",
                if rules.profiles().is_empty() {
                    "none".to_string()
                } else {
                    rules.profiles().keys().cloned().collect::<Vec<_>>().join(", ")
                }
            );
            return Ok(exit::FAILURE);
        }

        let set = match once {
            true => hosts.set_once(mac, Some(profile)).await?,
            false => hosts.pin(mac, Some(profile)).await?,
        };

        if !set {
            eprintln!(
                "No machine with the address {mac} has been seen, so there is no row to pin.\n\
                 A machine appears here the first time it asks to boot."
            );
            return Ok(exit::FAILURE);
        }

        match once {
            true => println!("{mac}: next boot only, `{profile}`."),
            false => println!("{mac}: pinned to `{profile}` until cleared."),
        }
        Ok(exit::SUCCESS)
    }
}

/// `pxe:tag` — label a machine so a rule can find it.
#[derive(Debug, Default)]
pub struct TagCommand;

#[async_trait]
impl Command for TagCommand {
    fn name(&self) -> &str {
        "pxe:tag"
    }

    fn description(&self) -> &str {
        "Add or remove tags on a machine"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:tag --mac=… --add=lab,dell\n  \
             pxe:tag --mac=… --remove=lab\n\n\
             Tags are what a rule's `tag = [\"…\"]` condition matches, so this is how a\n\
             machine is carved out of a policy without naming its address in the file.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let Some(mac) = mac_argument(args) else { return Ok(exit::FAILURE) };
        let hosts = app.resolve::<HostRepository>()?;

        let Some(mut host) = hosts.by_mac(mac).await? else {
            eprintln!("No machine with the address {mac} has been seen.");
            return Ok(exit::FAILURE);
        };

        let mut changed = false;
        for tag in split(args.option("add")) {
            changed |= host.add_tag(&tag);
        }
        for tag in split(args.option("remove")) {
            changed |= host.remove_tag(&tag);
        }

        if changed {
            hosts.save(&host).await?;
        }

        println!(
            "{mac}: {}",
            if host.tags.0.is_empty() { "no tags".to_string() } else { host.tags.0.join(", ") }
        );
        Ok(exit::SUCCESS)
    }
}

/// `pxe:log` — what has been happening.
#[derive(Debug, Default)]
pub struct LogCommand;

#[async_trait]
impl Command for LogCommand {
    fn name(&self) -> &str {
        "pxe:log"
    }

    fn description(&self) -> &str {
        "Show recent boot events"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:log [--mac=…] [--limit=N]\n\n\
             One row per thing that happened: an offer, a file read, a script served,\n\
             a refusal. `--mac` narrows it to one machine, which is the shape of the\n\
             question \"why did this not boot\".",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let events = app.resolve::<BootEventRepository>()?;
        let limit: u64 = args.parsed_or("limit", 50u64).clamp(1, 1000);

        let rows = match args.option("mac") {
            Some(_) => {
                let Some(mac) = mac_argument(args) else { return Ok(exit::FAILURE) };
                events.for_mac(mac, limit).await?
            }
            None => events.recent(limit).await?,
        };

        if rows.is_empty() {
            println!("Nothing yet.");
            return Ok(exit::SUCCESS);
        }

        io::table(
            &["WHEN", "MAC", "WHAT", "PROFILE", "FROM", "RULE", "DETAIL"],
            &rows
                .iter()
                .map(|event| {
                    vec![
                        event.at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        event.mac.clone(),
                        event.kind.clone(),
                        event.profile.clone().unwrap_or_else(|| "—".into()),
                        event.source.clone().unwrap_or_else(|| "—".into()),
                        event.rule.clone().unwrap_or_else(|| "—".into()),
                        event.detail.clone().unwrap_or_default(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        Ok(exit::SUCCESS)
    }
}

fn mac_argument(args: &Arguments) -> Option<MacAddr> {
    let raw = match args.option("mac") {
        Some(raw) => raw,
        None => {
            eprintln!("This command needs --mac=…");
            return None;
        }
    };

    match raw.parse() {
        Ok(mac) => Some(mac),
        Err(e) => {
            eprintln!("{e}");
            None
        }
    }
}

/// `--add=lab,dell` and `--add=lab --add=dell` should both work, and so should
/// a stray space after a comma.
fn split(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_list_is_read_the_way_people_type_it() {
        assert_eq!(split(Some("lab,dell")), vec!["lab", "dell"]);
        assert_eq!(split(Some("lab, dell ")), vec!["lab", "dell"]);
        assert_eq!(split(Some("lab,,")), vec!["lab"]);
        assert!(split(None).is_empty());
        assert!(split(Some("")).is_empty());
    }
}
