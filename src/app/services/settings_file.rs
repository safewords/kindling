//! Editing `kindling.toml` without flattening it, and a catalogue of what may
//! go in it.
//!
//! A settings form that reads the file into a map and writes the map back out
//! loses the comments, the blank lines and the order somebody grouped things
//! into. So values are changed *in place* with `toml_edit` — the key changes,
//! nothing else moves — and a key that was not there is added to its section.
//!
//! Only service settings live here (see `config::service_file`). The boot
//! policy is in the database, and this editor refuses a policy table the same
//! way the server does.
//!
//! The catalogue below is the other half. A configuration editor that shows
//! `PXE_LOOP_THRESHOLD` as a text box and nothing else has moved the problem
//! from "which variable do I set" to "what does this box do", so every setting
//! carries what it is for, what happens if it is wrong, and what it defaults
//! to.

use serde::Serialize;

use rainier_framework::config::{Config, Env};

use crate::config::service_file::{self, ValueKind};

/// What kind of value a setting takes, so an editor can render the right
/// control and refuse the obviously wrong thing before the server has to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Text,
    Integer,
    Boolean,
    /// An IPv4 address.
    Address,
    /// A filesystem path.
    Path,
    /// Never echoed back to a client in full.
    Secret,
    /// One of a closed set.
    Choice,
}

/// One row of the catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct Spec {
    pub key: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    /// What it is for, and what goes wrong when it is wrong.
    pub help: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<&'static [&'static str]>,
}

const fn spec(
    key: &'static str,
    section: &'static str,
    label: &'static str,
    kind: Kind,
    default: &'static str,
    help: &'static str,
) -> Spec {
    Spec {
        key,
        section,
        label,
        kind,
        default,
        help,
        choices: None,
    }
}

/// Every setting this server reads, in the order an editor should show them.
///
/// Ordered by how often somebody needs to change it, not alphabetically: the
/// address machines come back to is the first thing anybody sets and the TFTP
/// window size is the last.
pub const CATALOGUE: &[Spec] = &[
    // --- where this server is ---------------------------------------------
    Spec {
        key: "PXE_SERVER_IP",
        section: "Where machines reach this server",
        label: "Server address",
        kind: Kind::Address,
        default: "detected from the routing table",
        help: "Every URL a machine is told to fetch, and the `siaddr` of every DHCP reply, is \
               built from this. Wrong here is not a degraded server — it is a rack that \
               downloads nothing and says nothing about why. Left blank, the address of the \
               interface that reaches the default route is used and logged; in production, with \
               DHCP on, it must be stated.",
        choices: None,
    },
    spec(
        "PXE_HTTP_BASE",
        "Where machines reach this server",
        "HTTP base URL",
        Kind::Text,
        "http://<server address>:<port>",
        "The base every generated URL is built on. Set it when something sits in front of this \
         server — a reverse proxy, TLS, or a name machines resolve instead of an address.",
    ),
    spec(
        "PXE_DESCRIPTION",
        "Where machines reach this server",
        "Menu description",
        Kind::Text,
        "kindling netboot",
        "The line a PXE client shows on its own screen while it is deciding.",
    ),
    // --- the listeners -----------------------------------------------------
    spec(
        "SERVER_HOST",
        "Listeners",
        "HTTP bind address",
        Kind::Address,
        "0.0.0.0",
        "Not 127.0.0.1: a boot server that only answers itself is not a boot server, and the \
         machines that need it are by definition elsewhere.",
    ),
    spec(
        "SERVER_PORT",
        "Listeners",
        "HTTP port",
        Kind::Integer,
        "8080",
        "Where the admin interface, the API and the generated scripts are served.",
    ),
    spec(
        "PXE_DHCP_ENABLED",
        "Listeners",
        "Answer DHCP",
        Kind::Boolean,
        "true",
        "Proxy DHCP on ports 67 and 4011. This hands out no addresses — it answers the boot half \
         of the conversation beside whatever already does DHCP. Turn it off when something else \
         is already serving boot options.",
    ),
    spec(
        "PXE_DHCP_BIND",
        "Listeners",
        "DHCP bind address",
        Kind::Address,
        "0.0.0.0",
        "Which interface to answer boot requests on.",
    ),
    spec(
        "PXE_DHCP_BOOT_SERVER_PORT",
        "Listeners",
        "Listen on 4011",
        Kind::Boolean,
        "true",
        "The PXE boot server port. Some firmware insists on asking there rather than using the \
         file it was already offered; leaving this off makes those machines hang.",
    ),
    spec(
        "PXE_TFTP_ENABLED",
        "Listeners",
        "Serve TFTP",
        Kind::Boolean,
        "true",
        "Read-only TFTP on port 69, which is how firmware fetches the boot loader. Only turn it \
         off if another TFTP server already serves the same directory.",
    ),
    spec(
        "PXE_TFTP_PORT",
        "Listeners",
        "TFTP port",
        Kind::Integer,
        "69",
        "Firmware has this number built in; changing it is for testing.",
    ),
    // --- files -------------------------------------------------------------
    spec(
        "PXE_TFTP_ROOT",
        "Files",
        "Boot root",
        Kind::Path,
        "tftproot",
        "The directory served over TFTP and, under /boot/, over HTTP. The boot loaders and any \
         images live here. A path that resolves outside it is refused on both protocols.",
    ),
    spec(
        "PXE_OUI_FILE",
        "Files",
        "Vendor list",
        Kind::Path,
        "(the built-in table only)",
        "An IEEE `oui.txt`, or a `prefix,vendor[,class]` CSV, for vendor names beyond the \
         built-in table. Optional: rules matching on `vendor` only see what is loaded.",
    ),
    // --- policy behaviour --------------------------------------------------
    spec(
        "PXE_LOOP_THRESHOLD",
        "Chainload loop detection",
        "Transactions before a loop is called",
        Kind::Integer,
        "4",
        "The backstop for iPXE that announces itself neither by user class nor by its own \
         options — a relay stripped them, or the build never set them. Counted in whole DHCP \
         transactions, not requests, so firmware retransmitting one request is never mistaken \
         for a loop. Below 2 turns it off.",
    ),
    spec(
        "PXE_LOOP_WINDOW_SECS",
        "Chainload loop detection",
        "Window",
        Kind::Integer,
        "90",
        "How long those transactions have to fall inside. A loop iteration is a DHCP round, a \
         fetch and an iPXE start — five to ten seconds — so ninety comfortably holds four.",
    ),
    // --- storage and access ------------------------------------------------
    Spec {
        key: "PXE_API_TOKEN",
        section: "Access and storage",
        label: "API token",
        kind: Kind::Secret,
        default: "(unset: writing endpoints are closed)",
        help: "Guards everything that changes what a machine will boot. Unset means those \
               endpoints are CLOSED, not open — this server decides what a fleet executes at \
               power-on, and the convenient default would be a remote-code-execution primitive \
               on a boot network. Reading the inventory never needs it.",
        choices: None,
    },
    spec(
        "DB_DATABASE",
        "Access and storage",
        "Inventory database",
        Kind::Path,
        "storage/pxe.sqlite",
        "Where the machines this server has seen, and the boot log, are kept. SQLite on disk, so \
         it survives a restart.",
    ),
    Spec {
        key: "DATABASE_URL",
        section: "Access and storage",
        label: "Database URL",
        kind: Kind::Secret,
        default: "(the SQLite file above)",
        help: "Point this at Postgres or MySQL and nothing else changes. It carries a password, \
               so it is never echoed back in full.",
        choices: None,
    },
    spec(
        "PXE_EVENT_RETENTION_DAYS",
        "Access and storage",
        "Keep boot events for",
        Kind::Integer,
        "30",
        "The boot log is the only thing here that grows without bound. Older rows are pruned \
         nightly.",
    ),
    // --- the application ---------------------------------------------------
    Spec {
        key: "APP_ENV",
        section: "Application",
        label: "Environment",
        kind: Kind::Choice,
        default: "production",
        help: "Unset means production, which is the right default for a deployment — several \
               checks refuse in production where they would otherwise guess, including the \
               server address above.",
        choices: Some(&["local", "testing", "staging", "production"]),
    },
    spec(
        "APP_NAME",
        "Application",
        "Name",
        Kind::Text,
        "kindling",
        "What this installation calls itself, in the admin interface and in log lines. Worth \
         setting when there is more than one of these on a network and a log line has to say \
         which.",
    ),
    Spec {
        key: "APP_KEY",
        section: "Application",
        label: "Application key",
        kind: Kind::Secret,
        default: "(generated per start)",
        help: "Signs and encrypts anything this server needs to hand out and trust back. \
               Generated on each start when unset, which means anything signed stops verifying \
               after a restart. `key:generate` prints one.",
        choices: None,
    },
    Spec {
        key: "RUST_LOG",
        section: "Application",
        label: "Log filter",
        kind: Kind::Text,
        default: "info,sqlx=warn",
        help: "`sqlx` logs every statement at info, and a rack coming up would bury the lines \
               that say which image each machine was sent.",
        choices: None,
    },
    Spec {
        key: "LOG_FORMAT",
        section: "Application",
        label: "Log format",
        kind: Kind::Choice,
        default: "auto",
        help: "`auto` is JSON in production and readable everywhere else.",
        choices: Some(&["auto", "json", "pretty"]),
    },
];

pub fn spec_for(key: &str) -> Option<&'static Spec> {
    CATALOGUE.iter().find(|spec| spec.key == key)
}

/// A setting as an editor should see it.
#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    #[serde(flatten)]
    pub spec: &'static Spec,
    /// Where it is written in `kindling.toml`: `server.ip`.
    pub path: &'static str,
    /// What the file says, or `None` when the file does not mention it.
    pub value: Option<String>,
    /// Whether the value is withheld because it is a secret.
    pub redacted: bool,
}

/// Why a proposed file was not accepted.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsError {
    pub problems: Vec<String>,
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problems.join("; "))
    }
}

impl std::error::Error for SettingsError {}

fn problem(message: impl Into<String>) -> SettingsError {
    SettingsError {
        problems: vec![message.into()],
    }
}

/// `kindling.toml`, edited in place.
pub struct SettingsFile {
    path: std::path::PathBuf,
}

impl SettingsFile {
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file this server was started with.
    pub fn current() -> Self {
        Self::at(service_file::path())
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The file's text, or empty when there is no file yet.
    ///
    /// Empty rather than an error: every setting has a default, and the
    /// editor is how a file gets written.
    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    /// The catalogue, filled in from the file.
    ///
    /// Read from the file rather than from the running configuration, because
    /// an editor edits the file — showing the live value would quietly present
    /// a detected address as though somebody had typed it.
    pub fn settings(&self) -> Vec<Setting> {
        let declared = service_file::parse(&self.read()).unwrap_or_default();

        CATALOGUE
            .iter()
            .filter_map(|spec| {
                let key = service_file::key_for_variable(spec.key)?;
                let value = declared
                    .iter()
                    .find(|(variable, _)| *variable == spec.key)
                    .map(|(_, value)| value.clone());
                let redacted = spec.kind == Kind::Secret && value.is_some();
                Some(Setting {
                    spec,
                    path: key.path,
                    value: if redacted {
                        Some(mask(value.as_deref().unwrap_or_default()))
                    } else {
                        value
                    },
                    redacted,
                })
            })
            .collect()
    }

    /// Check that a proposed file would boot, without writing it.
    ///
    /// The same parser and the same `configure` the server runs, so this
    /// answers the question that matters — "would this start" — rather than a
    /// similar one that drifts.
    pub fn validate(text: &str) -> Result<(), SettingsError> {
        let settings = service_file::parse(text).map_err(|e| SettingsError {
            problems: e.problems,
        })?;
        let env = Env::from_map(settings.into_iter().map(|(k, v)| (k.to_string(), v)));
        let config = Config::new();
        crate::config::configure(&config, &env).map_err(|e| problem(e.message().to_string()))
    }

    /// Replace the whole file after validating it, keeping a copy of the old
    /// one beside it.
    pub fn write(&self, text: &str) -> Result<(), SettingsError> {
        Self::validate(text)?;
        self.save(text)
    }

    /// Change some settings, named as the server reads them (`PXE_SERVER_IP`),
    /// leaving every comment and ordering decision in the file where it was.
    /// A value of `None` removes the key, so the default applies again.
    pub fn apply(&self, changes: &[(String, Option<String>)]) -> Result<String, SettingsError> {
        let edited = rewrite(&self.read(), changes)?;
        Self::validate(&edited)?;
        self.save(&edited)?;
        Ok(edited)
    }

    fn save(&self, text: &str) -> Result<(), SettingsError> {
        if self.path.exists() {
            let backup = self.path.with_extension("toml.bak");
            std::fs::copy(&self.path, &backup).map_err(|e| {
                problem(format!("could not back up to `{}`: {e}", backup.display()))
            })?;
        }
        std::fs::write(&self.path, text)
            .map_err(|e| problem(format!("could not write `{}`: {e}", self.path.display())))
    }
}

/// Apply changes to the text of a `kindling.toml`, preserving everything else.
fn rewrite(original: &str, changes: &[(String, Option<String>)]) -> Result<String, SettingsError> {
    let mut document: toml_edit::DocumentMut = original
        .parse()
        .map_err(|e| problem(format!("the file is not valid TOML: {e}")))?;

    for (variable, value) in changes {
        let key = service_file::key_for_variable(variable)
            .ok_or_else(|| problem(format!("this server reads no setting called `{variable}`")))?;
        let (section, name) = key.path.split_once('.').expect("paths have a section");

        match value {
            None => {
                if let Some(table) = document.get_mut(section).and_then(|t| t.as_table_mut()) {
                    table.remove(name);
                }
            }
            Some(value) => {
                let item = match key.kind {
                    ValueKind::Integer => {
                        toml_edit::value(value.trim().parse::<i64>().map_err(|_| {
                            problem(format!(
                                "`{}` must be a whole number, not `{value}`",
                                key.path
                            ))
                        })?)
                    }
                    ValueKind::Boolean => toml_edit::value(match value.trim() {
                        "true" | "1" | "yes" | "on" => true,
                        "false" | "0" | "no" | "off" => false,
                        other => {
                            return Err(problem(format!(
                                "`{}` must be true or false, not `{other}`",
                                key.path
                            )))
                        }
                    }),
                    ValueKind::Text => toml_edit::value(value.as_str()),
                };
                if document.get(section).is_none() {
                    document.insert(section, toml_edit::Item::Table(toml_edit::Table::new()));
                }
                let table = document[section]
                    .as_table_mut()
                    .ok_or_else(|| problem(format!("`{section}` in the file is not a section")))?;
                table.insert(name, item);
            }
        }
    }

    Ok(document.to_string())
}

/// Enough of a secret to recognise it, not enough to use it.
fn mask(value: &str) -> String {
    match value.chars().count() {
        0 => String::new(),
        n if n <= 8 => "•".repeat(n),
        n => format!("{}…{}", &value[..2], "•".repeat(n.min(32) - 2)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "# kindling\n\
                            [server]\n\
                            # The address machines come back to.\n\
                            ip = \"10.0.0.2\"\n\
                            \n\
                            [http]\n\
                            port = 8080 # the admin UI too\n";

    #[test]
    fn changing_a_value_leaves_every_comment_where_it_was() {
        let edited = rewrite(ORIGINAL, &[("SERVER_PORT".into(), Some("80".into()))]).unwrap();
        assert!(edited.contains("# kindling"), "{edited}");
        assert!(
            edited.contains("# The address machines come back to."),
            "{edited}"
        );
        assert!(edited.contains("port = 80"), "{edited}");
        assert!(edited.contains("ip = \"10.0.0.2\""), "{edited}");
    }

    #[test]
    fn a_new_setting_goes_into_its_section() {
        let edited = rewrite(
            ORIGINAL,
            &[("PXE_DHCP_ENABLED".into(), Some("false".into()))],
        )
        .unwrap();
        let settings = service_file::parse(&edited).unwrap();
        assert!(
            settings.contains(&("PXE_DHCP_ENABLED", "false".into())),
            "{edited}"
        );
    }

    #[test]
    fn removing_a_setting_puts_the_default_back() {
        let edited = rewrite(ORIGINAL, &[("SERVER_PORT".into(), None)]).unwrap();
        assert!(!edited.contains("port ="), "{edited}");
    }

    #[test]
    fn a_setting_nobody_reads_is_refused() {
        assert!(rewrite(ORIGINAL, &[("PXE_RULES_PATH".into(), Some("x".into()))]).is_err());
    }

    #[test]
    fn a_proposed_file_is_checked_against_the_loader_that_would_run_it() {
        let error = SettingsFile::validate("[server]\nip = \"boot.example.com\"\n").unwrap_err();
        assert!(error.problems[0].contains("four bytes"), "{error}");

        let error = SettingsFile::validate("[profiles.local]\nkind = \"local\"\n").unwrap_err();
        assert!(error.problems[0].contains("database"), "{error}");
    }

    #[test]
    fn the_catalogue_explains_every_setting_and_has_no_duplicates() {
        let mut keys: Vec<&str> = CATALOGUE.iter().map(|spec| spec.key).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before, "a setting is listed twice");

        for spec in CATALOGUE {
            assert!(
                service_file::key_for_variable(spec.key).is_some(),
                "`{}` has nowhere to go in kindling.toml",
                spec.key
            );
            assert!(
                spec.help.len() > 40,
                "`{}` needs a real explanation",
                spec.key
            );
            assert!(!spec.label.is_empty(), "`{}` needs a label", spec.key);
            if spec.kind == Kind::Choice {
                assert!(
                    spec.choices.is_some(),
                    "`{}` is a choice of what?",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn a_secret_is_recognisable_but_not_usable() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("short"), "•••••");

        let masked = mask("a6b0d43342c25def80a3ee79bea1007f");
        assert!(masked.starts_with("a6"), "{masked}");
        assert!(!masked.contains("1007f"), "{masked}");
    }

    #[test]
    fn the_editor_reads_values_out_of_the_file_and_hides_the_secrets() {
        let path =
            std::env::temp_dir().join(format!("kindling-settings-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[server]\nip = \"10.0.0.2\"\n[api]\ntoken = \"supersecretvalue\"\n",
        )
        .unwrap();

        let settings = SettingsFile::at(&path).settings();
        let address = settings
            .iter()
            .find(|s| s.spec.key == "PXE_SERVER_IP")
            .unwrap();
        assert_eq!(address.value.as_deref(), Some("10.0.0.2"));
        assert_eq!(address.path, "server.ip");
        assert!(!address.redacted);

        let token = settings
            .iter()
            .find(|s| s.spec.key == "PXE_API_TOKEN")
            .unwrap();
        assert!(token.redacted);
        assert!(!token
            .value
            .as_deref()
            .unwrap_or_default()
            .contains("supersecret"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_file_reads_as_empty_rather_than_failing() {
        let file = SettingsFile::at(std::env::temp_dir().join("kindling-settings-not-here.toml"));
        assert_eq!(file.read(), "");
        assert!(file.settings().iter().all(|s| s.value.is_none()));
    }
}
