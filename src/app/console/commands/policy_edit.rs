//! `pxe:rule`, `pxe:profile`, `pxe:policy`, `pxe:db:backup` — changing the
//! boot policy from a terminal.
//!
//! The policy lives in the database. These commands change it the same way
//! the web UI and the API do — through [`RuleStore::apply`], so a change is
//! validated as a whole policy before anything is stored, recorded as a
//! revision (`pxe:rules --history`, `--restore`), and refused, with every
//! problem listed, if it would not load. A running `pxe:serve` notices the new
//! revision within a few seconds; nothing needs restarting.
//!
//! There is deliberately no "apply this file". Each command changes one thing
//! and says what it changed, so the history reads as a list of decisions
//! rather than "the file was imported again".
//!
//! ```sh
//! pxe pxe:profile set rescue --kernel='{{boot}}/vmlinuz' --initrd='{{boot}}/initrd.img' \
//!     --cmdline='ip=dhcp'
//! pxe pxe:rule add lab-rescue --profile=rescue --when=mac=18:66:da:11:22:33 --priority=300
//! pxe pxe:rule set lab-rescue --no-profile
//! pxe pxe:policy default hands-off
//! ```

use std::collections::BTreeMap;
use std::io::Read;

use rainier_framework::console_kernel::{exit, Arguments, Command};
use rainier_framework::database::Database;
use rainier_framework::prelude::*;
use serde_json::{Map, Value};

use crate::app::services::{EditError, EditMeta, RuleStore};
use crate::pxe::profile::Profile;
use crate::pxe::rules::{PolicyDocument, Rule};

const ACTOR: &str = "console";

/// Say what happened, the same way for every command.
async fn change(
    app: &Application,
    summary: String,
    edit: impl FnOnce(&mut PolicyDocument) -> std::result::Result<(), EditError>,
) -> Result<i32> {
    let store = app.resolve::<RuleStore>()?;
    Ok(
        match store.apply(EditMeta::new(&summary, ACTOR), edit).await {
            Ok(applied) => {
                match applied.revision {
                    Some(revision) => println!(
                    "{summary}: revision {}. A running pxe:serve picks it up within a few seconds.",
                    revision.id
                ),
                    None => println!("Nothing changed: the policy already says that."),
                }
                exit::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}\n\nNothing changed; the policy is as it was.");
                exit::FAILURE
            }
        },
    )
}

fn invalid(problem: impl Into<String>) -> EditError {
    EditError::invalid(problem)
}

fn usage(command: &str, help: &str) -> Result<i32> {
    eprintln!("{help}\n\n(`pxe help {command}` says the same.)");
    Ok(exit::FAILURE)
}

// ---------------------------------------------------------------------------
// Conditions from the command line.
// ---------------------------------------------------------------------------

/// `FACT=VALUE` pairs into the condition a rule's `when` holds.
///
/// Each pair is one fact; all of them must hold. A value is a list of
/// alternatives separated by commas (`vendor=Dell,Lenovo*`), `true` or
/// `false`, or a whole number — the shape the facts take (`known=false`,
/// `boots_at_least=3`, `time_between=22:00,06:00`).
pub fn condition_from_pairs(pairs: &[&str]) -> std::result::Result<Value, String> {
    let mut map = Map::new();
    for pair in pairs {
        let Some((fact, value)) = pair.split_once('=') else {
            return Err(format!(
                "`{pair}` is not FACT=VALUE (for example `mac=18:66:da:11:22:33` or \
                 `vendor=Dell,Lenovo*`)"
            ));
        };
        let fact = fact.trim();
        if fact.is_empty() {
            return Err(format!("`{pair}` names no fact"));
        }
        let value = value.trim();
        let parsed = match value {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            v if !v.is_empty() && v.parse::<i64>().is_ok() => {
                Value::Number(v.parse::<i64>().expect("checked").into())
            }
            v => Value::Array(
                v.split(',')
                    .map(|s| Value::String(s.trim().to_string()))
                    .filter(|s| s.as_str() != Some(""))
                    .collect(),
            ),
        };
        if map.insert(fact.to_string(), parsed).is_some() {
            return Err(format!(
                "`{fact}` is given twice; put the alternatives in one value, separated by commas"
            ));
        }
    }
    Ok(Value::Object(map))
}

/// `--when` / `--when-json` / `--always`, or the `unless` equivalents.
fn condition_option(
    args: &Arguments,
    name: &str,
) -> std::result::Result<Option<Option<Value>>, String> {
    let pairs = args.option_values(name);
    let json = args.option(&format!("{name}-json"));
    let cleared = match name {
        "when" => args.flag("always"),
        _ => args.flag("no-unless"),
    };
    match (pairs.is_empty(), json, cleared) {
        (true, None, false) => Ok(None),
        (false, None, false) => Ok(Some(Some(condition_from_pairs(&pairs)?))),
        (true, Some(json), false) => serde_json::from_str(json)
            .map(|v| Some(Some(v)))
            .map_err(|e| format!("`--{name}-json` is not JSON: {e}")),
        (true, None, true) => Ok(Some(None)),
        _ => Err(format!(
            "give one of `--{name}=…`, `--{name}-json=…` or `--{}`",
            if name == "when" {
                "always"
            } else {
                "no-unless"
            }
        )),
    }
}

// ---------------------------------------------------------------------------
// pxe:rule
// ---------------------------------------------------------------------------

const RULE_HELP: &str = "Usage:\n  \
    pxe:rule add NAME [options]       Add a rule\n  \
    pxe:rule set NAME [options]       Change a rule (only what is given)\n  \
    pxe:rule enable NAME | disable NAME\n  \
    pxe:rule remove NAME\n  \
    pxe:rule show NAME\n\n\
    Options:\n  \
    --profile=NAME          Boot this profile        --no-profile   choose none\n  \
    --priority=N            Higher runs first\n  \
    --when=FACT=VALUE       Repeatable; all must hold. VALUE: a,b,c | true | false | N\n  \
    --when-json=JSON        A full condition tree    --always       match every machine\n  \
    --unless=FACT=VALUE     Exclusions, like --when  --no-unless    drop them\n  \
    --tag=T                 Repeatable: tags to add (replaces the list)\n  \
    --remove-tag=T          Repeatable: tags to take off\n  \
    --set=KEY=VALUE         Repeatable: variables for later rules and the profile\n  \
    --stop | --continue     Whether evaluation ends when it fires\n  \
    --description=TEXT      --rename=NEW (set only)   --disabled (add only)\n\n\
    Facts: mac, vendor, hostname, uuid, manufacturer, product, serial, asset, arch,\n\
    device_class, subnet, tag, known, boots_at_least, time_between, weekday, var.…\n\
    `pxe:test --mac=…` shows what a machine would boot afterwards.";

#[derive(Debug, Default)]
pub struct RuleCommand;

#[async_trait]
impl Command for RuleCommand {
    fn name(&self) -> &str {
        "pxe:rule"
    }

    fn description(&self) -> &str {
        "Add, change, enable, disable or remove a boot policy rule"
    }

    fn help(&self) -> Option<&str> {
        Some(RULE_HELP)
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let (Some(action), Some(name)) = (args.argument(0), args.argument(1)) else {
            return usage("pxe:rule", RULE_HELP);
        };
        let name = name.to_string();

        match action {
            "show" => {
                let rules = app.resolve::<RuleStore>()?.current();
                let Some(rule) = rules.rules().iter().find(|r| r.name == name) else {
                    eprintln!("no rule is called `{name}`");
                    return Ok(exit::FAILURE);
                };
                let (when, unless) = rules.describe(&name).unwrap_or_default();
                println!("{name}{}", if rule.enabled { "" } else { " (off)" });
                if let Some(d) = &rule.description {
                    println!("  {d}");
                }
                println!("  priority  {}", rule.priority);
                println!("  when      {when}");
                if let Some(unless) = unless {
                    println!("  unless    {unless}");
                }
                println!("  does      {}", super::rules::describe_actions(rule));
                println!("  stops     {}", if rule.stops() { "yes" } else { "no" });
                Ok(exit::SUCCESS)
            }
            "enable" | "disable" => {
                let on = action == "enable";
                change(app, format!("rule `{name}` {action}d"), move |doc| {
                    let rule = doc.rule_mut(&name).ok_or_else(|| {
                        EditError::not_found(format!("no rule is called `{name}`"))
                    })?;
                    rule.enabled = on;
                    Ok(())
                })
                .await
            }
            "remove" => {
                change(app, format!("rule `{name}` removed"), move |doc| {
                    let before = doc.rules.len();
                    doc.rules.retain(|r| r.name != name);
                    if doc.rules.len() == before {
                        return Err(EditError::not_found(format!("no rule is called `{name}`")));
                    }
                    Ok(())
                })
                .await
            }
            "add" => {
                let edits = match rule_edits(args, true) {
                    Ok(edits) => edits,
                    Err(e) => return fail(e),
                };
                change(app, format!("rule `{name}` added"), move |doc| {
                    if doc.rule(&name).is_some() {
                        return Err(invalid(format!(
                            "a rule called `{name}` exists; `pxe:rule set {name} …` changes it"
                        )));
                    }
                    let mut value = serde_json::json!({ "name": name });
                    apply_edits(&mut value, &edits);
                    doc.rules.push(rule_from(value)?);
                    Ok(())
                })
                .await
            }
            "set" => {
                let edits = match rule_edits(args, false) {
                    Ok(edits) if edits.is_empty() && args.option("rename").is_none() => {
                        return usage("pxe:rule", RULE_HELP)
                    }
                    Ok(edits) => edits,
                    Err(e) => return fail(e),
                };
                let rename = args.option("rename").map(str::to_string);
                let summary = match &rename {
                    Some(new) => format!("rule `{name}` updated and renamed to `{new}`"),
                    None => format!("rule `{name}` updated"),
                };
                change(app, summary, move |doc| {
                    if let Some(new) = &rename {
                        if new != &name && doc.rule(new).is_some() {
                            return Err(invalid(format!("a rule called `{new}` exists")));
                        }
                    }
                    let position =
                        doc.rules
                            .iter()
                            .position(|r| r.name == name)
                            .ok_or_else(|| {
                                EditError::not_found(format!("no rule is called `{name}`"))
                            })?;
                    let mut value = serde_json::to_value(&doc.rules[position])
                        .map_err(|e| invalid(e.to_string()))?;
                    apply_edits(&mut value, &edits);
                    if let Some(new) = rename {
                        value["name"] = Value::String(new);
                    }
                    doc.rules[position] = rule_from(value)?;
                    Ok(())
                })
                .await
            }
            other => {
                eprintln!("`{other}` is not something pxe:rule does.");
                usage("pxe:rule", RULE_HELP)
            }
        }
    }
}

fn fail(problem: String) -> Result<i32> {
    eprintln!("{problem}\n\nNothing changed.");
    Ok(exit::FAILURE)
}

/// One field to set (`Some`) or remove (`None`) on a rule or profile.
type Edits = Vec<(&'static str, Option<Value>)>;

fn apply_edits(target: &mut Value, edits: &Edits) {
    let object = target
        .as_object_mut()
        .expect("rules and profiles are objects");
    for (field, value) in edits {
        match value {
            Some(value) => {
                object.insert((*field).to_string(), value.clone());
            }
            None => {
                object.remove(*field);
            }
        }
    }
}

fn rule_from(value: Value) -> std::result::Result<Rule, EditError> {
    serde_json::from_value(value).map_err(|e| invalid(format!("the rule is not valid: {e}")))
}

/// What the options say to change on a rule.
pub fn rule_edits(args: &Arguments, adding: bool) -> std::result::Result<Edits, String> {
    let mut edits: Edits = Vec::new();

    if let Some(description) = args.option("description") {
        edits.push((
            "description",
            (!description.is_empty()).then(|| Value::String(description.into())),
        ));
    }
    if let Some(priority) = args.option("priority") {
        let n: i64 = priority
            .parse()
            .map_err(|_| format!("`--priority={priority}` is not a whole number"))?;
        edits.push(("priority", Some(n.into())));
    }
    match (args.option("profile"), args.flag("no-profile")) {
        (Some(_), true) => return Err("give `--profile=…` or `--no-profile`, not both".into()),
        (Some(profile), false) => edits.push(("profile", Some(Value::String(profile.into())))),
        (None, true) => edits.push(("profile", None)),
        (None, false) => {}
    }
    if let Some(when) = condition_option(args, "when")? {
        // Absent is "every machine", which is what `--always` means.
        edits.push(("when", when));
    }
    if let Some(unless) = condition_option(args, "unless")? {
        edits.push(("unless", unless));
    }
    for (option, field) in [("tag", "tag"), ("remove-tag", "remove_tags")] {
        let values = args.option_values(option);
        if !values.is_empty() {
            edits.push((
                field,
                Some(values.iter().map(|v| Value::String((*v).into())).collect()),
            ));
        }
    }
    if args.flag("no-tags") {
        edits.push(("tag", None));
        edits.push(("remove_tags", None));
    }
    let sets = args.option_values("set");
    if !sets.is_empty() {
        let mut map = BTreeMap::new();
        for pair in sets {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("`--set={pair}` is not KEY=VALUE"))?;
            map.insert(key.trim().to_string(), Value::String(value.to_string()));
        }
        edits.push(("set", Some(Value::Object(map.into_iter().collect()))));
    } else if args.flag("no-set") {
        edits.push(("set", None));
    }
    match (args.flag("stop"), args.flag("continue")) {
        (true, true) => return Err("give `--stop` or `--continue`, not both".into()),
        (true, false) => edits.push(("stop", Some(Value::Bool(true)))),
        (false, true) => edits.push(("stop", Some(Value::Bool(false)))),
        _ => {}
    }
    if adding && args.flag("disabled") {
        edits.push(("enabled", Some(Value::Bool(false))));
    }
    Ok(edits)
}

// ---------------------------------------------------------------------------
// pxe:profile
// ---------------------------------------------------------------------------

const PROFILE_HELP: &str = "Usage:\n  \
    pxe:profile set NAME [options]    Create a profile, or change one (only what is given)\n  \
    pxe:profile remove NAME\n  \
    pxe:profile show NAME\n\n\
    Options (an empty value, as in --kernel=, removes the field):\n  \
    --kind=kernel|script|menu|local|ignore\n  \
    --label=TEXT  --description=TEXT\n  \
    --kernel=URL  --initrd=URL (repeatable)  --cmdline=TEXT\n  \
    --script=TEXT            iPXE by hand; --script=- reads it once from stdin\n  \
    --entry=PROFILE[:KEY]    Repeatable: a menu's entries, in order\n  \
    --timeout=SECONDS  --default=PROFILE      (menus)\n  \
    --bootfile=ARCH=FILE     Repeatable: a stage-one loader for this profile only\n\n\
    Templates: {{boot}} {{base}} {{mac}} {{hostname}} {{ var.NAME }} …\n\
    `pxe:test --mac=… --ipxe` shows the script a machine would be sent.";

#[derive(Debug, Default)]
pub struct ProfileCommand;

#[async_trait]
impl Command for ProfileCommand {
    fn name(&self) -> &str {
        "pxe:profile"
    }

    fn description(&self) -> &str {
        "Create, change or remove a boot profile"
    }

    fn help(&self) -> Option<&str> {
        Some(PROFILE_HELP)
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let (Some(action), Some(name)) = (args.argument(0), args.argument(1)) else {
            return usage("pxe:profile", PROFILE_HELP);
        };
        let name = name.to_string();

        match action {
            "show" => {
                let rules = app.resolve::<RuleStore>()?.current();
                let Some(profile) = rules.profiles().get(&name) else {
                    eprintln!("no profile is called `{name}`");
                    return Ok(exit::FAILURE);
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(profile).unwrap_or_default()
                );
                let users: Vec<&str> = rules
                    .rules()
                    .iter()
                    .filter(|r| r.profile.as_deref() == Some(name.as_str()))
                    .map(|r| r.name.as_str())
                    .collect();
                println!(
                    "\nChosen by: {}",
                    if users.is_empty() {
                        "no rule".to_string()
                    } else {
                        users.join(", ")
                    }
                );
                Ok(exit::SUCCESS)
            }
            "remove" => {
                change(app, format!("profile `{name}` removed"), move |doc| {
                    if doc.profiles.remove(&name).is_none() {
                        return Err(EditError::not_found(format!(
                            "no profile is called `{name}`"
                        )));
                    }
                    Ok(())
                })
                .await
            }
            "set" => {
                let edits = match profile_edits(args) {
                    Ok(edits) if edits.is_empty() => return usage("pxe:profile", PROFILE_HELP),
                    Ok(edits) => edits,
                    Err(e) => return fail(e),
                };
                let summary = format!("profile `{name}` saved");
                change(app, summary, move |doc| {
                    let mut value = match doc.profiles.get(&name) {
                        Some(existing) => {
                            serde_json::to_value(existing).map_err(|e| invalid(e.to_string()))?
                        }
                        None => Value::Object(Map::new()),
                    };
                    apply_edits(&mut value, &edits);
                    let profile: Profile = serde_json::from_value(value)
                        .map_err(|e| invalid(format!("the profile is not valid: {e}")))?;
                    doc.profiles.insert(name, profile);
                    Ok(())
                })
                .await
            }
            other => {
                eprintln!("`{other}` is not something pxe:profile does.");
                usage("pxe:profile", PROFILE_HELP)
            }
        }
    }
}

/// What the options say to change on a profile.
pub fn profile_edits(args: &Arguments) -> std::result::Result<Edits, String> {
    let mut edits: Edits = Vec::new();
    let text = |v: &str| (!v.is_empty()).then(|| Value::String(v.into()));

    for (option, field) in [
        ("kind", "kind"),
        ("label", "label"),
        ("description", "description"),
        ("kernel", "kernel"),
        ("cmdline", "cmdline"),
        ("default", "default"),
    ] {
        if let Some(value) = args.option(option) {
            edits.push((field, text(value)));
        }
    }
    if let Some(script) = args.option("script") {
        let script = if script == "-" {
            let mut read = String::new();
            std::io::stdin()
                .read_to_string(&mut read)
                .map_err(|e| format!("could not read the script from stdin: {e}"))?;
            read
        } else {
            script.to_string()
        };
        edits.push(("script", text(&script)));
    }
    let initrds = args.option_values("initrd");
    if !initrds.is_empty() {
        let list: Vec<Value> = initrds
            .iter()
            .filter(|v| !v.is_empty())
            .map(|v| Value::String((*v).into()))
            .collect();
        edits.push(("initrd", (!list.is_empty()).then_some(Value::Array(list))));
    }
    if let Some(timeout) = args.option("timeout") {
        if timeout.is_empty() {
            edits.push(("timeout", None));
        } else {
            let n: u64 = timeout
                .parse()
                .map_err(|_| format!("`--timeout={timeout}` is not a number of seconds"))?;
            edits.push(("timeout", Some(n.into())));
        }
    }
    let entries = args.option_values("entry");
    if !entries.is_empty() {
        let list: Vec<Value> = entries
            .iter()
            .filter(|v| !v.is_empty())
            .map(|entry| match entry.split_once(':') {
                Some((profile, key)) => serde_json::json!({ "profile": profile, "key": key }),
                None => serde_json::json!({ "profile": entry }),
            })
            .collect();
        edits.push(("entries", (!list.is_empty()).then_some(Value::Array(list))));
    }
    let bootfiles = args.option_values("bootfile");
    if !bootfiles.is_empty() {
        let mut map = Map::new();
        for pair in bootfiles {
            if pair.is_empty() {
                continue;
            }
            let (arch, file) = pair
                .split_once('=')
                .ok_or_else(|| format!("`--bootfile={pair}` is not ARCH=FILE"))?;
            map.insert(arch.to_string(), Value::String(file.to_string()));
        }
        edits.push(("bootfile", (!map.is_empty()).then_some(Value::Object(map))));
    }
    Ok(edits)
}

// ---------------------------------------------------------------------------
// pxe:policy
// ---------------------------------------------------------------------------

const POLICY_HELP: &str = "Usage:\n  \
    pxe:policy default PROFILE        What a machine boots when no rule chooses\n  \
    pxe:policy timezone MINUTES       Offset from UTC for `time_between` and `weekday`\n  \
    pxe:policy bootloader ARCH FILE   The stage-one loader for an architecture\n  \
    pxe:policy bootloader ARCH --remove\n\n\
    ARCH: bios, x86-uefi, x64-uefi, arm64-uefi, … or `default`.\n\
    `pxe:rules` shows the whole policy; `pxe:rules --history` its revisions.";

#[derive(Debug, Default)]
pub struct PolicyCommand;

#[async_trait]
impl Command for PolicyCommand {
    fn name(&self) -> &str {
        "pxe:policy"
    }

    fn description(&self) -> &str {
        "Set the default profile, the timezone, or a boot loader"
    }

    fn help(&self) -> Option<&str> {
        Some(POLICY_HELP)
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        match (args.argument(0), args.argument(1), args.argument(2)) {
            (Some("default"), Some(profile), None) => {
                let profile = profile.to_string();
                change(
                    app,
                    format!("the default profile is now `{profile}`"),
                    move |doc| {
                        doc.settings.default_profile = Some(profile);
                        Ok(())
                    },
                )
                .await
            }
            (Some("timezone"), Some(minutes), None) => {
                let Ok(minutes) = minutes.parse::<i64>() else {
                    return fail(format!("`{minutes}` is not a number of minutes"));
                };
                change(
                    app,
                    format!("the timezone offset is now {minutes} minutes"),
                    move |doc| {
                        doc.settings.timezone_offset_minutes = minutes;
                        Ok(())
                    },
                )
                .await
            }
            (Some("bootloader"), Some(arch), file) => {
                let arch = arch.to_string();
                match (file, args.flag("remove")) {
                    (Some(file), false) => {
                        let file = file.to_string();
                        change(
                            app,
                            format!("the boot loader for `{arch}` is now `{file}`"),
                            move |doc| {
                                doc.bootloaders.insert(arch, file);
                                Ok(())
                            },
                        )
                        .await
                    }
                    (None, true) => {
                        change(
                            app,
                            format!("the boot loader for `{arch}` was removed"),
                            move |doc| {
                                doc.bootloaders.remove(&arch).ok_or_else(|| {
                                    EditError::not_found(format!(
                                        "there is no boot loader for `{arch}`"
                                    ))
                                })?;
                                Ok(())
                            },
                        )
                        .await
                    }
                    _ => usage("pxe:policy", POLICY_HELP),
                }
            }
            _ => usage("pxe:policy", POLICY_HELP),
        }
    }
}

// ---------------------------------------------------------------------------
// pxe:db:backup
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct BackupCommand;

#[async_trait]
impl Command for BackupCommand {
    fn name(&self) -> &str {
        "pxe:db:backup"
    }

    fn description(&self) -> &str {
        "Copy the database — policy, revisions, inventory — to a file"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  pxe:db:backup --to=PATH\n\n\
             A consistent copy of the SQLite database, taken while the server runs (VACUUM \
             INTO). It is a database, not a policy file: restore it by pointing \
             `[database] path` at it. With Postgres or MySQL, use that server's own backups.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let Some(to) = args.option("to") else {
            eprintln!("say where: `pxe:db:backup --to=backups/pxe.sqlite`");
            return Ok(exit::FAILURE);
        };
        if to.contains('\'') {
            eprintln!("a path with a quote in it is not supported here");
            return Ok(exit::FAILURE);
        }
        if std::path::Path::new(to).exists() {
            eprintln!("`{to}` exists; choose a new name rather than overwrite a backup");
            return Ok(exit::FAILURE);
        }
        let database = app.resolve::<Database>()?;
        if !matches!(database.dialect(), rainier_orm::Dialect::Sqlite) {
            eprintln!("this backs up SQLite only; use your database server's own tools");
            return Ok(exit::FAILURE);
        }
        database.statement(&format!("VACUUM INTO '{to}'")).await?;
        println!("Wrote `{to}`.");
        Ok(exit::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(argv: &[&str]) -> Arguments {
        Arguments::parse(argv.iter().copied())
    }

    #[test]
    fn pairs_become_a_condition_with_the_types_the_facts_take() {
        let condition = condition_from_pairs(&[
            "mac=18:66:da:11:22:33,18:66:da:11:22:34",
            "known=false",
            "boots_at_least=3",
        ])
        .unwrap();
        assert_eq!(
            condition,
            serde_json::json!({
                "mac": ["18:66:da:11:22:33", "18:66:da:11:22:34"],
                "known": false,
                "boots_at_least": 3,
            })
        );
        assert!(condition_from_pairs(&["mac"]).is_err());
        assert!(condition_from_pairs(&["mac=a", "mac=b"]).is_err());
    }

    #[test]
    fn a_rule_built_from_options_is_a_rule_the_policy_accepts() {
        let edits = rule_edits(
            &args(&[
                "pxe:rule",
                "add",
                "r",
                "--profile=local",
                "--priority=300",
                "--when=mac=18:66:da:11:22:33",
                "--tag=rescue",
                "--set=ip=10.20.0.9",
            ]),
            true,
        )
        .unwrap();
        let mut value = serde_json::json!({ "name": "r" });
        apply_edits(&mut value, &edits);
        let rule = rule_from(value).unwrap();

        let mut document = PolicyDocument::starter();
        document.rules.push(rule);
        let rules =
            crate::pxe::rules::RuleSet::from_document(document, crate::pxe::rules::Origin::Inline)
                .expect("it loads");
        let rule = &rules.rules()[0];
        assert_eq!(rule.profile.as_deref(), Some("local"));
        assert_eq!(rule.priority, 300);
        assert_eq!(rule.tag, vec!["rescue"]);
        assert_eq!(rule.set.get("ip").map(String::as_str), Some("10.20.0.9"));
    }

    #[test]
    fn clearing_and_conflicting_options_say_what_they_mean() {
        let edits = rule_edits(
            &args(&["pxe:rule", "set", "r", "--no-profile", "--always"]),
            false,
        )
        .unwrap();
        assert!(edits.contains(&("profile", None)));
        assert!(edits.contains(&("when", None)));

        assert!(rule_edits(
            &args(&["pxe:rule", "set", "r", "--profile=a", "--no-profile"]),
            false
        )
        .is_err());
        assert!(rule_edits(
            &args(&["pxe:rule", "set", "r", "--stop", "--continue"]),
            false
        )
        .is_err());
    }

    #[test]
    fn a_profile_built_from_options_is_a_profile() {
        let edits = profile_edits(&args(&[
            "pxe:profile",
            "set",
            "rescue",
            "--kernel={{boot}}/vmlinuz",
            "--initrd={{boot}}/a.img",
            "--initrd={{boot}}/b.img",
            "--cmdline=ip=dhcp",
        ]))
        .unwrap();
        let mut value = Value::Object(Map::new());
        apply_edits(&mut value, &edits);
        let profile: Profile = serde_json::from_value(value).unwrap();
        assert_eq!(profile.kernel.as_deref(), Some("{{boot}}/vmlinuz"));
        assert_eq!(profile.initrd.len(), 2);
        assert_eq!(profile.cmdline.as_deref(), Some("ip=dhcp"));

        // An empty value removes the field.
        let edits = profile_edits(&args(&["pxe:profile", "set", "rescue", "--cmdline="])).unwrap();
        assert!(edits.contains(&("cmdline", None)));
    }
}
