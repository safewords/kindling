//! Editing `.env` without flattening it, and a catalogue of what may go in it.
//!
//! The same problem the policy file has: a settings form that reads the file
//! into a map and writes the map back out loses the comments, the blank lines
//! and the order somebody grouped things into. So values are replaced *in
//! place* — the line changes, nothing else moves — and a key that was not
//! there is appended rather than sorted in.
//!
//! The catalogue below is the other half. A configuration editor that shows
//! `PXE_LOOP_THRESHOLD` as a text box and nothing else has moved the problem
//! from "which variable do I set" to "what does this box do", so every setting
//! carries what it is for, what happens if it is wrong, and what it defaults
//! to.

use serde::Serialize;

use rainier_framework::config::{Config, Env};

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
    Spec { key, section, label, kind, default, help, choices: None }
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
        "PXE_RULES_PATH",
        "Files",
        "Legacy policy file",
        Kind::Path,
        "pxe-rules.toml",
        "The boot policy lives in the database and is edited on the Policy screen. A TOML \
         policy file from an earlier release found here is imported once, on the first start \
         against an empty database, and never read again.",
    ),
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
    /// What `.env` says, or `None` when the file does not mention it.
    pub value: Option<String>,
    /// Whether the value is withheld because it is a secret.
    pub redacted: bool,
}

/// Why a proposed `.env` was not accepted.
#[derive(Debug, Clone, Serialize)]
pub struct EnvError {
    pub problems: Vec<String>,
}

impl std::fmt::Display for EnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problems.join("; "))
    }
}

impl std::error::Error for EnvError {}

/// `.env`, edited in place.
pub struct EnvFile {
    path: std::path::PathBuf,
}

impl EnvFile {
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The file's text, or empty when there is no file yet.
    ///
    /// Empty rather than an error: a fresh install has no `.env` and the
    /// editor is how one gets written.
    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    /// The catalogue, filled in from the file.
    ///
    /// Read from the file rather than from the running configuration, because
    /// an editor edits the file — showing the live value would quietly present
    /// a detected address as though somebody had typed it.
    pub fn settings(&self) -> Vec<Setting> {
        let declared = parse(&self.read());

        CATALOGUE
            .iter()
            .map(|spec| {
                let value = declared
                    .iter()
                    .find(|(key, _)| key == spec.key)
                    .map(|(_, value)| value.clone());

                let redacted = spec.kind == Kind::Secret && value.is_some();
                Setting {
                    spec,
                    value: if redacted { Some(mask(value.as_deref().unwrap_or_default())) } else { value },
                    redacted,
                }
            })
            .collect()
    }

    /// Check that a proposed file would boot, without writing it.
    ///
    /// The same `configure` the server runs, so this answers the question that
    /// matters — "would this start" — rather than a similar one that drifts.
    pub fn validate(text: &str) -> Result<(), EnvError> {
        let env = Env::parse(text).isolated();
        let config = Config::new();

        crate::config::configure(&config, &env)
            .map_err(|e| EnvError { problems: vec![e.message().to_string()] })
    }

    /// Replace the whole file after validating it, keeping a copy of the old
    /// one beside it.
    pub fn write(&self, text: &str) -> Result<(), EnvError> {
        Self::validate(text)?;
        self.save(text)
    }

    /// Change some values, leaving every comment, blank line and ordering
    /// decision in the file exactly where it was.
    ///
    /// A value of `None` comments the line out rather than deleting it, which
    /// is what somebody turning a setting off almost always means — and it
    /// leaves the name visible for whoever comes looking for it.
    pub fn apply(
        &self,
        changes: &[(String, Option<String>)],
    ) -> Result<String, EnvError> {
        let edited = rewrite(&self.read(), changes);
        Self::validate(&edited)?;
        self.save(&edited)?;
        Ok(edited)
    }

    fn save(&self, text: &str) -> Result<(), EnvError> {
        if self.path.exists() {
            let backup = self.path.with_extension("bak");
            std::fs::copy(&self.path, &backup).map_err(|e| EnvError {
                problems: vec![format!("could not back up to `{}`: {e}", backup.display())],
            })?;
        }

        std::fs::write(&self.path, text).map_err(|e| EnvError {
            problems: vec![format!("could not write `{}`: {e}", self.path.display())],
        })
    }
}

/// Apply changes to the text of a `.env`, preserving everything else.
fn rewrite(original: &str, changes: &[(String, Option<String>)]) -> String {
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let mut appended: Vec<String> = Vec::new();

    for (key, value) in changes {
        let existing = lines.iter().position(|line| declares(line, key));

        match (existing, value) {
            (Some(index), Some(value)) => lines[index] = format!("{key}={}", quote(value)),
            // Commented out rather than removed: the name stays where the
            // person who set it will look for it.
            (Some(index), None) => lines[index] = format!("#{key}="),
            (None, Some(value)) => appended.push(format!("{key}={}", quote(value))),
            (None, None) => {}
        }
    }

    if !appended.is_empty() {
        if !lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("# Added by the configuration editor.".to_string());
        lines.extend(appended);
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Whether a line sets this key. A commented-out line does not.
fn declares(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Quote a value only when it needs it — an unquoted file is easier to read,
/// and most values are a word.
fn quote(value: &str) -> String {
    let needs = value.is_empty()
        || value.starts_with(' ')
        || value.ends_with(' ')
        || value.contains('#')
        || value.contains('"');

    if needs {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// The `KEY=value` pairs a file declares, ignoring comments.
fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .map(|v| v.replace("\\\"", "\"").replace("\\\\", "\\"))
                .unwrap_or_else(|| value.to_string());
            Some((key.trim().to_string(), value))
        })
        .collect()
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
                            # The address machines come back to.\n\
                            PXE_SERVER_IP=10.0.0.2\n\
                            \n\
                            # Listeners\n\
                            SERVER_PORT=8080\n\
                            #PXE_OUI_FILE=oui.txt\n";

    #[test]
    fn changing_a_value_leaves_every_comment_where_it_was() {
        // The reason this is not a map round-trip.
        let edited = rewrite(ORIGINAL, &[("SERVER_PORT".into(), Some("80".into()))]);

        assert!(edited.contains("# kindling"));
        assert!(edited.contains("# The address machines come back to."));
        assert!(edited.contains("# Listeners"));
        assert!(edited.contains("SERVER_PORT=80"));
        assert!(!edited.contains("SERVER_PORT=8080"));
        assert!(edited.contains("PXE_SERVER_IP=10.0.0.2"), "untouched settings stay untouched");
    }

    #[test]
    fn a_commented_out_line_is_not_a_declaration() {
        // `#PXE_OUI_FILE=` is a suggestion, not a setting, so setting it adds
        // a line rather than silently reviving the commented one in place.
        assert!(!declares("#PXE_OUI_FILE=oui.txt", "PXE_OUI_FILE"));
        assert!(declares("PXE_OUI_FILE=oui.txt", "PXE_OUI_FILE"));
        assert!(declares("  PXE_OUI_FILE = oui.txt", "PXE_OUI_FILE"));

        // And a longer name that merely starts the same is a different key.
        assert!(!declares("PXE_TFTP_PORT=69", "PXE_TFTP"));
    }

    #[test]
    fn a_new_setting_is_appended_under_a_heading_that_says_where_it_came_from() {
        let edited = rewrite(ORIGINAL, &[("PXE_API_TOKEN".into(), Some("s3cret".into()))]);

        assert!(edited.contains("# Added by the configuration editor."));
        assert!(edited.trim_end().ends_with("PXE_API_TOKEN=s3cret"));
        assert!(edited.contains("SERVER_PORT=8080"), "nothing else moved");
    }

    #[test]
    fn turning_a_setting_off_comments_it_rather_than_deleting_it() {
        // The name stays where whoever set it will go looking for it.
        let edited = rewrite(ORIGINAL, &[("PXE_SERVER_IP".into(), None)]);

        assert!(edited.contains("#PXE_SERVER_IP="));
        assert!(!edited.contains("PXE_SERVER_IP=10.0.0.2"));
        assert!(edited.contains("# The address machines come back to."), "its comment survives");
    }

    #[test]
    fn a_value_is_quoted_only_when_it_needs_to_be() {
        assert_eq!(quote("10.0.0.2"), "10.0.0.2");
        assert_eq!(quote("info,sqlx=warn"), "info,sqlx=warn");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a # b"), "\"a # b\"");
        assert_eq!(quote(" padded "), "\" padded \"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn a_proposed_file_is_checked_against_the_loader_that_would_run_it() {
        // Not a similar check that drifts: the same `configure`.
        assert!(EnvFile::validate("APP_ENV=local\nSERVER_PORT=8080\n").is_ok());

        let error = EnvFile::validate("APP_ENV=local\nPXE_SERVER_IP=not-an-address\n").unwrap_err();
        assert!(error.to_string().contains("PXE_SERVER_IP"), "{error}");

        // Unset `APP_ENV` means production, where guessing the address is
        // refused — so the editor catches it before the restart does.
        let error = EnvFile::validate("SERVER_PORT=8080\n").unwrap_err();
        assert!(error.to_string().contains("PXE_SERVER_IP is not set"), "{error}");
    }

    #[test]
    fn the_catalogue_explains_every_setting_and_has_no_duplicates() {
        let mut keys: Vec<&str> = CATALOGUE.iter().map(|spec| spec.key).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before, "a setting is listed twice");

        for spec in CATALOGUE {
            assert!(spec.help.len() > 40, "`{}` needs a real explanation", spec.key);
            assert!(!spec.label.is_empty(), "`{}` needs a label", spec.key);
            if spec.kind == Kind::Choice {
                assert!(spec.choices.is_some(), "`{}` is a choice of what?", spec.key);
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
        let path = std::env::temp_dir().join(format!("kindling-env-{}.env", std::process::id()));
        std::fs::write(&path, "PXE_SERVER_IP=10.0.0.2\nPXE_API_TOKEN=supersecretvalue\n").unwrap();

        let settings = EnvFile::at(&path).settings();
        let address = settings.iter().find(|s| s.spec.key == "PXE_SERVER_IP").unwrap();
        assert_eq!(address.value.as_deref(), Some("10.0.0.2"));
        assert!(!address.redacted);

        let token = settings.iter().find(|s| s.spec.key == "PXE_API_TOKEN").unwrap();
        assert!(token.redacted);
        assert!(!token.value.as_deref().unwrap_or_default().contains("supersecret"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_file_reads_as_empty_rather_than_failing() {
        // A fresh install has no `.env`, and the editor is how one gets one.
        let env = EnvFile::at(std::env::temp_dir().join("kindling-env-not-here.env"));
        assert_eq!(env.read(), "");
        assert!(env.settings().iter().all(|s| s.value.is_none()));
    }
}
