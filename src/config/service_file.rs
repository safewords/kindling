//! `kindling.toml` — how this server is hosted, and nothing else.
//!
//! Where it listens, which address machines are told to come back to, where
//! the database and the boot files are, the API token. Those are facts about
//! the machine kindling runs on, set once by whoever installs it, and a file is
//! the right home for them.
//!
//! The boot policy is not one of them. Rules, profiles, the default profile
//! and the boot loaders live in the database and only there: edited in the web
//! UI, through the API, or with the `pxe:rule`, `pxe:profile` and `pxe:policy`
//! commands, every change validated against the inventory and recorded as a
//! revision that can be rolled back. A policy kept in a text file is a second
//! source of truth that drifts from the first, so this file refuses one — a
//! `[profiles]` table here is an error that says where the policy lives, not a
//! table that is quietly ignored.
//!
//! Every key maps onto one of the environment variables the configuration
//! sections read (`[server] ip` is `PXE_SERVER_IP`), and a variable that is
//! really set in the process environment wins over the file. That is what a
//! container or a systemd unit wants: the file describes the install, and a
//! secret can still come from the environment.
//!
//! ```toml
//! [server]
//! ip = "10.0.0.2"
//!
//! [http]
//! port = 8080
//!
//! [api]
//! token = "…"          # openssl rand -hex 32
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rainier_framework::config::Env;

/// The file read when nothing says otherwise.
pub const DEFAULT_PATH: &str = "kindling.toml";

/// The variable that names a different file.
pub const PATH_VARIABLE: &str = "KINDLING_CONFIG";

/// What a key holds, so a value of the wrong type is refused with a sentence
/// rather than turned into a string and refused later, somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Text,
    Integer,
    Boolean,
}

/// One key of the file.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// `section.key`, as written in the file.
    pub path: &'static str,
    /// The setting it becomes.
    pub variable: &'static str,
    pub kind: ValueKind,
}

const fn key(path: &'static str, variable: &'static str, kind: ValueKind) -> Key {
    Key {
        path,
        variable,
        kind,
    }
}

use ValueKind::{Boolean, Integer, Text};

/// Every key the file may contain, in the order a new file lists them.
pub const KEYS: &[Key] = &[
    key("server.ip", "PXE_SERVER_IP", Text),
    key("server.http_base", "PXE_HTTP_BASE", Text),
    key("server.description", "PXE_DESCRIPTION", Text),
    key("http.host", "SERVER_HOST", Text),
    key("http.port", "SERVER_PORT", Integer),
    key("http.max_body", "SERVER_MAX_BODY", Integer),
    key("http.request_timeout", "SERVER_REQUEST_TIMEOUT", Integer),
    key("http.compression", "SERVER_COMPRESSION", Boolean),
    key("dhcp.enabled", "PXE_DHCP_ENABLED", Boolean),
    key("dhcp.bind", "PXE_DHCP_BIND", Text),
    key(
        "dhcp.boot_server_port",
        "PXE_DHCP_BOOT_SERVER_PORT",
        Boolean,
    ),
    key("tftp.enabled", "PXE_TFTP_ENABLED", Boolean),
    key("tftp.bind", "PXE_TFTP_BIND", Text),
    key("tftp.port", "PXE_TFTP_PORT", Integer),
    key("tftp.root", "PXE_TFTP_ROOT", Text),
    key("tftp.max_block_size", "PXE_TFTP_MAX_BLOCK_SIZE", Integer),
    key("tftp.max_window_size", "PXE_TFTP_MAX_WINDOW_SIZE", Integer),
    key("smb.enabled", "PXE_SMB_ENABLED", Boolean),
    key("smb.bind", "PXE_SMB_BIND", Text),
    key("smb.port", "PXE_SMB_PORT", Integer),
    key("smb.user", "PXE_SMB_USER", Text),
    key("smb.password", "PXE_SMB_PASSWORD", Text),
    key("smb.shares", "PXE_SMB_SHARES", Text),
    key("loop_detection.threshold", "PXE_LOOP_THRESHOLD", Integer),
    key(
        "loop_detection.window_secs",
        "PXE_LOOP_WINDOW_SECS",
        Integer,
    ),
    key("api.token", "PXE_API_TOKEN", Text),
    key("database.url", "DATABASE_URL", Text),
    key("database.path", "DB_DATABASE", Text),
    key(
        "database.event_retention_days",
        "PXE_EVENT_RETENTION_DAYS",
        Integer,
    ),
    key("inventory.oui_file", "PXE_OUI_FILE", Text),
    key("app.name", "APP_NAME", Text),
    key("app.env", "APP_ENV", Text),
    key("app.debug", "APP_DEBUG", Boolean),
    key("app.url", "APP_URL", Text),
    key("app.key", "APP_KEY", Text),
    key("app.previous_keys", "APP_PREVIOUS_KEYS", Text),
    key("log.format", "LOG_FORMAT", Text),
    key("log.filter", "RUST_LOG", Text),
];

/// The key a setting is written under, if the file can hold it.
pub fn key_for_variable(variable: &str) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.variable == variable)
}

/// Top-level names that would make this file a policy. Refused by name, so
/// the message can say where the policy is instead of "unknown key".
const POLICY_TABLES: &[&str] = &[
    "rule",
    "rules",
    "profile",
    "profiles",
    "bootloaders",
    "settings",
    "policy",
    "default_profile",
];

/// Why a file was not accepted. Every problem, not the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub problems: Vec<String>,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.problems.as_slice() {
            [one] => f.write_str(one),
            many => {
                writeln!(f, "{} problems:", many.len())?;
                for problem in many {
                    writeln!(f, "  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for FileError {}

/// The message for a policy table, shared with the configuration editor.
pub fn policy_refusal(name: &str) -> String {
    format!(
        "`{name}` is boot policy, and the boot policy is not read from files: it lives in the \
         database, where every change is validated and kept as a revision. Use the web UI, the \
         API, or `pxe:rule`, `pxe:profile` and `pxe:policy` (`pxe help pxe:rule`). This file \
         holds only how the server is hosted."
    )
}

/// The settings a file declares, as `(variable, value)` pairs.
pub fn parse(text: &str) -> Result<Vec<(&'static str, String)>, FileError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| FileError {
        problems: vec![format!("not valid TOML: {e}")],
    })?;

    let mut problems = Vec::new();
    let mut settings = Vec::new();

    for (section, value) in &table {
        if POLICY_TABLES.contains(&section.as_str()) {
            problems.push(policy_refusal(section));
            continue;
        }
        let Some(inner) = value.as_table() else {
            problems.push(format!(
                "`{section}` is a value at the top level; every setting belongs to a section, \
                 as in `[server]` then `ip = \"10.0.0.2\"`"
            ));
            continue;
        };
        for (name, value) in inner {
            let path = format!("{section}.{name}");
            let Some(key) = KEYS.iter().find(|key| key.path == path) else {
                problems.push(unknown(&path));
                continue;
            };
            match render(key, value) {
                Ok(rendered) => settings.push((key.variable, rendered)),
                Err(problem) => problems.push(problem),
            }
        }
    }

    if problems.is_empty() {
        Ok(settings)
    } else {
        Err(FileError { problems })
    }
}

fn unknown(path: &str) -> String {
    let section = path.split('.').next().unwrap_or_default();
    let known: Vec<&str> = KEYS
        .iter()
        .filter(|key| key.path.starts_with(&format!("{section}.")))
        .map(|key| key.path)
        .collect();
    if known.is_empty() {
        let sections: Vec<&str> = {
            let mut s: Vec<&str> = KEYS
                .iter()
                .map(|key| key.path.split('.').next().unwrap_or_default())
                .collect();
            s.dedup();
            s
        };
        format!(
            "`[{section}]` is not a section of this file; the sections are {}",
            sections
                .iter()
                .map(|s| format!("`[{s}]`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            "`{path}` is not a setting; `[{section}]` holds {}",
            known
                .iter()
                .map(|k| format!("`{}`", k.rsplit('.').next().unwrap_or(k)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn render(key: &Key, value: &toml::Value) -> Result<String, String> {
    match (key.kind, value) {
        (Text, toml::Value::String(s)) => Ok(s.clone()),
        (Integer, toml::Value::Integer(i)) => Ok(i.to_string()),
        (Boolean, toml::Value::Boolean(b)) => Ok(b.to_string()),
        // A port written as a string is a common slip and means one thing.
        (Integer, toml::Value::String(s)) if s.trim().parse::<i64>().is_ok() => {
            Ok(s.trim().to_string())
        }
        (kind, other) => Err(format!(
            "`{}` must be {}, not {}",
            key.path,
            match kind {
                Text => "a string",
                Integer => "a whole number",
                Boolean => "true or false",
            },
            other.type_str()
        )),
    }
}

/// The file this process reads: `KINDLING_CONFIG`, or `kindling.toml`.
pub fn path() -> PathBuf {
    std::env::var_os(PATH_VARIABLE)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH))
}

/// Read a file into the layer the configuration sections read, under the
/// process environment. A missing file is an empty layer: every setting has a
/// default that works on a laptop.
pub fn load(path: &Path) -> Result<Env, FileError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Env::new()),
        Err(e) => {
            return Err(FileError {
                problems: vec![format!("could not read `{}`: {e}", path.display())],
            })
        }
    };
    let settings = parse(&text).map_err(|e| FileError {
        problems: e
            .problems
            .into_iter()
            .map(|p| format!("`{}`: {p}", path.display()))
            .collect(),
    })?;

    let mut env = Env::new();
    for (variable, value) in settings {
        env.set(variable, value);
    }
    Ok(env)
}

/// A `kindling.toml` holding what a `.env` from an earlier release said.
///
/// The settings it recognises become keys; anything else is listed in a
/// comment at the end rather than dropped, so nothing disappears without a
/// trace. `PXE_RULES_PATH` is not carried over: it named a policy file, and
/// there are none any more.
pub fn from_dotenv(text: &str) -> String {
    let env = Env::parse(text).isolated();
    let mut sections: BTreeMap<&str, Vec<(&str, String)>> = BTreeMap::new();
    let mut leftovers = Vec::new();

    for (variable, value) in env.file_vars() {
        match key_for_variable(variable) {
            Some(key) => {
                let (section, name) = key.path.split_once('.').expect("paths have a section");
                sections
                    .entry(section)
                    .or_default()
                    .push((name, literal(key, value)));
            }
            None if variable == "PXE_RULES_PATH" => {}
            None => leftovers.push(variable.clone()),
        }
    }

    let mut out = String::from(
        "# kindling's service settings: how this server is hosted.\n\
         # Converted from .env. The boot policy is not configured here: it lives in the\n\
         # database (web UI, API, or `pxe:rule` / `pxe:profile` / `pxe:policy`).\n",
    );
    // In the order the catalogue lists sections, not alphabetically.
    let mut order: Vec<&str> = KEYS
        .iter()
        .map(|key| key.path.split('.').next().unwrap_or_default())
        .collect();
    order.dedup();
    for section in order {
        let Some(mut entries) = sections.remove(section) else {
            continue;
        };
        entries.sort_by_key(|(name, _)| {
            KEYS.iter()
                .position(|key| key.path == format!("{section}.{name}"))
                .unwrap_or(usize::MAX)
        });
        out.push_str(&format!("\n[{section}]\n"));
        for (name, value) in entries {
            out.push_str(&format!("{name} = {value}\n"));
        }
    }
    if !leftovers.is_empty() {
        leftovers.sort();
        out.push_str(
            "\n# These were in .env but are not settings of this file. Export them in the\n\
             # environment if something still needs them:\n",
        );
        for variable in leftovers {
            out.push_str(&format!("#   {variable}\n"));
        }
    }
    out
}

fn literal(key: &Key, value: &str) -> String {
    match key.kind {
        Integer if value.trim().parse::<i64>().is_ok() => value.trim().to_string(),
        Boolean if matches!(value.trim(), "true" | "false") => value.trim().to_string(),
        Boolean if matches!(value.trim(), "1" | "yes" | "on") => "true".into(),
        Boolean if matches!(value.trim(), "0" | "no" | "off") => "false".into(),
        _ => toml::Value::String(value.to_string()).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_becomes_the_settings_the_sections_read() {
        let settings =
            parse("[server]\nip = \"10.0.0.2\"\n[http]\nport = 9090\n[dhcp]\nenabled = false\n")
                .unwrap();
        assert!(settings.contains(&("PXE_SERVER_IP", "10.0.0.2".into())));
        assert!(settings.contains(&("SERVER_PORT", "9090".into())));
        assert!(settings.contains(&("PXE_DHCP_ENABLED", "false".into())));
    }

    #[test]
    fn a_policy_in_the_file_is_refused_with_where_the_policy_lives() {
        let error = parse(
            "[profiles.local]\nkind = \"local\"\n\n[[rule]]\nname = \"x\"\n[settings]\ndefault_profile = \"local\"\n",
        )
        .unwrap_err();
        assert_eq!(error.problems.len(), 3, "{error}");
        for problem in &error.problems {
            assert!(problem.contains("lives in the"), "{problem}");
            assert!(problem.contains("pxe:rule"), "{problem}");
        }
    }

    #[test]
    fn an_unknown_key_names_the_ones_that_exist() {
        let error = parse("[server]\naddress = \"10.0.0.2\"\n").unwrap_err();
        assert!(error.problems[0].contains("`server.address`"), "{error}");
        assert!(error.problems[0].contains("`ip`"), "{error}");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        let error = parse("[http]\nport = true\n").unwrap_err();
        assert!(error.problems[0].contains("whole number"), "{error}");
    }

    #[test]
    fn every_key_is_reachable_by_its_setting_and_unique() {
        for key in KEYS {
            assert_eq!(key_for_variable(key.variable).unwrap().path, key.path);
            assert_eq!(KEYS.iter().filter(|k| k.path == key.path).count(), 1);
        }
    }

    #[test]
    fn a_dotenv_converts_and_reads_back_to_the_same_settings() {
        let dotenv = "PXE_SERVER_IP=10.0.0.2\nSERVER_PORT=8081\nPXE_DHCP_ENABLED=true\n\
                      PXE_RULES_PATH=policy.toml\nPXE_API_TOKEN=abc\nSOMETHING_ELSE=1\n";
        let converted = from_dotenv(dotenv);
        assert!(!converted.contains("policy.toml"), "{converted}");
        assert!(
            converted.contains("SOMETHING_ELSE"),
            "kept as a comment: {converted}"
        );

        let settings = parse(&converted).unwrap();
        assert!(settings.contains(&("PXE_SERVER_IP", "10.0.0.2".into())));
        assert!(settings.contains(&("SERVER_PORT", "8081".into())));
        assert!(settings.contains(&("PXE_DHCP_ENABLED", "true".into())));
        assert!(settings.contains(&("PXE_API_TOKEN", "abc".into())));
    }

    #[test]
    fn a_missing_file_is_an_empty_layer() {
        let env = load(Path::new("definitely-not-here.toml")).unwrap();
        assert!(env.file_vars().is_empty());
    }
}
