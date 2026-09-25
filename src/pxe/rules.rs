//! The policy: which machine gets which image, and why.
//!
//! A policy is four things — settings, the stage-one boot loaders, the
//! profiles a machine can be sent, and the rules that choose between them —
//! and it lives in the database, edited from the web UI or the API. What is
//! in this module is the part that does not care where it lives: the
//! [`PolicyDocument`] every store reads and writes, the validation that
//! decides whether a document may run, and the evaluator.
//!
//! A rule is a condition tree (see [`condition`](super::condition)) and a set
//! of actions: choose a profile, add tags, remove tags, set variables, stop.
//! Rules run in priority order, highest first, and ties keep document order.
//! The first rule to choose a profile wins; rules that only tag or set
//! variables carry on, and what they did is visible to every rule after them
//! — which is how one rule classifies a machine and a later one acts on the
//! class.
//!
//! Evaluation records why every rule did or did not fire — see
//! [`Evaluation::trace`]. A boot policy that cannot explain itself is one
//! nobody dares change.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

use super::condition::{Compiled, Condition, Scope};
use super::facts::{ClientFacts, Stage};
use super::mac::MacAddr;
use super::pattern::{Cidr, Glob};
use super::profile::{Profile, ProfileKind};

/// An IEEE prefix as a rule writes it: `18:66:da`, `1866DA`, `18-66-da`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OuiPrefix([u8; 3]);

impl OuiPrefix {
    pub fn matches(&self, mac: MacAddr) -> bool {
        mac.oui() == self.0
    }
}

impl fmt::Display for OuiPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}:{:02x}:{:02x}", self.0[0], self.0[1], self.0[2])
    }
}

impl std::str::FromStr for OuiPrefix {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let cleaned: String = s.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();
        if cleaned.len() != 6 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "`{s}` is not an OUI: expected three octets like `18:66:da`"
            ));
        }
        let mut prefix = [0u8; 3];
        for (index, octet) in prefix.iter_mut().enumerate() {
            *octet = u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16)
                .map_err(|_| format!("`{s}` is not an OUI"))?;
        }
        Ok(OuiPrefix(prefix))
    }
}

impl Serialize for OuiPrefix {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OuiPrefix {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// A window of the day, possibly wrapping midnight.
///
/// Times are **UTC** unless the policy's `timezone_offset_minutes` says
/// otherwise. A server's idea of "local" is whatever its container image
/// inherited, which is not a thing to hang a reimage window on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "TimeWindowSpec")]
pub struct TimeWindow {
    from: NaiveTime,
    to: NaiveTime,
}

/// Written as the pair it was read as, `["22:00", "06:00"]`.
impl Serialize for TimeWindow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        [self.from.format("%H:%M").to_string(), self.to.format("%H:%M").to_string()]
            .serialize(serializer)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum TimeWindowSpec {
    Pair([String; 2]),
    Table { from: String, to: String },
}

impl TryFrom<TimeWindowSpec> for TimeWindow {
    type Error = String;

    fn try_from(spec: TimeWindowSpec) -> Result<Self, Self::Error> {
        let (from, to) = match spec {
            TimeWindowSpec::Pair([from, to]) => (from, to),
            TimeWindowSpec::Table { from, to } => (from, to),
        };
        Ok(TimeWindow { from: parse_time(&from)?, to: parse_time(&to)? })
    }
}

fn parse_time(text: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(text.trim(), "%H:%M:%S"))
        .map_err(|_| format!("`{text}` is not a time of day; write it as `22:00`"))
}

impl TimeWindow {
    pub fn contains(&self, at: DateTime<Utc>, offset_minutes: i64) -> bool {
        let shifted = at + Duration::minutes(offset_minutes);
        let now = NaiveTime::from_hms_opt(shifted.hour(), shifted.minute(), shifted.second())
            .unwrap_or(self.from);

        if self.from <= self.to {
            now >= self.from && now <= self.to
        } else {
            // Wraps midnight: `22:00` to `06:00` is two intervals.
            now >= self.from || now <= self.to
        }
    }
}

impl fmt::Display for TimeWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}–{}", self.from.format("%H:%M"), self.to.format("%H:%M"))
    }
}

/// The flat condition form of earlier releases: one key per fact, a list is
/// "any of these", and the keys present are ANDed.
///
/// Only ever read — on the way in, from a policy file written for an older
/// release — and turned straight into a [`Condition`] tree by
/// [`Match::into_condition`]. `deny_unknown_fields`, because `vendorr =
/// ["Dell"]` otherwise means "match everything": a rule that quietly reimages
/// the fleet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default)]
    pub mac: Vec<Glob>,
    #[serde(default)]
    pub oui: Vec<OuiPrefix>,
    #[serde(default)]
    pub vendor: Vec<Glob>,
    #[serde(default)]
    pub device_class: Vec<String>,
    #[serde(default)]
    pub arch: Vec<String>,
    #[serde(default)]
    pub vendor_class: Vec<Glob>,
    #[serde(default)]
    pub user_class: Vec<Glob>,
    #[serde(default)]
    pub hostname: Vec<Glob>,
    #[serde(default)]
    pub uuid: Vec<Glob>,
    #[serde(default)]
    pub manufacturer: Vec<Glob>,
    #[serde(default)]
    pub product: Vec<Glob>,
    #[serde(default)]
    pub serial: Vec<Glob>,
    #[serde(default)]
    pub asset: Vec<Glob>,
    #[serde(default)]
    pub subnet: Vec<Cidr>,
    #[serde(default)]
    pub tag: Vec<String>,
    #[serde(default)]
    pub stage: Option<Stage>,
    #[serde(default)]
    pub ipxe: Option<bool>,
    #[serde(default)]
    pub known: Option<bool>,
    #[serde(default)]
    pub boots_at_least: Option<u64>,
    #[serde(default)]
    pub boots_at_most: Option<u64>,
    #[serde(default)]
    pub time_between: Option<TimeWindow>,
    #[serde(default)]
    pub weekday: Vec<String>,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Turned off without being deleted.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Higher runs first. Ties keep document order.
    #[serde(default)]
    pub priority: i64,
    /// What must hold for the rule to fire. Absent is "every machine".
    #[serde(default)]
    pub when: Condition,
    /// If *this* holds, the rule does not fire. The readable way to write
    /// "every Dell except the two in the corner" — the same as a `not` inside
    /// `when`, but it reads as the exception it is, and the trace says
    /// "excluded" rather than "did not match".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<Condition>,

    // --- actions ---
    /// What the machine boots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Tags to attach to the machine in the inventory when this rule fires.
    #[serde(default, alias = "add_tags", skip_serializing_if = "Vec::is_empty")]
    pub tag: Vec<String>,
    /// Tags to take off it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_tags: Vec<String>,
    /// Variables: visible to every later rule as the `var` fact, and to the
    /// chosen profile's templates as `{{ var.name }}`. A later rule setting
    /// the same variable replaces the value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    /// Whether matching ends evaluation. Defaults to `true` for a rule that
    /// chooses a profile and `false` for one that does not — which is what
    /// both kinds are almost always for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<bool>,
}

fn yes() -> bool {
    true
}

impl Rule {
    pub fn stops(&self) -> bool {
        self.stop.unwrap_or(self.profile.is_some())
    }

    /// Whether this rule changes anything when it fires.
    pub fn acts(&self) -> bool {
        self.profile.is_some()
            || !self.tag.is_empty()
            || !self.remove_tags.is_empty()
            || !self.set.is_empty()
            || self.stop == Some(true)
    }
}

/// Policy-wide settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// What a machine boots when no rule chose anything.
    #[serde(default)]
    pub default_profile: Option<String>,
    /// Minutes to add to UTC before evaluating `time` and `weekday`.
    #[serde(default)]
    pub timezone_offset_minutes: i64,
}

/// A whole policy, as stored, exported and imported.
///
/// JSON is its native form — it is what the database holds and the API
/// speaks. TOML is read for importing a policy file from an earlier release,
/// which is why `[[rule]]` is still accepted beside `rules`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    #[serde(default)]
    pub settings: Settings,
    /// The stage-one boot file per architecture label, plus a `default`.
    #[serde(default)]
    pub bootloaders: BTreeMap<String, String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default, rename = "rules", alias = "rule")]
    pub rules: Vec<Rule>,
}

/// The name an earlier release gave the same thing.
pub type RuleFile = PolicyDocument;

impl PolicyDocument {
    pub fn from_toml(text: &str) -> Result<Self, LoadError> {
        toml::from_str(text)
            .map_err(|e| LoadError::one(format!("the policy will not parse as TOML: {e}")))
    }

    pub fn from_json(text: &str) -> Result<Self, LoadError> {
        serde_json::from_str(text)
            .map_err(|e| LoadError::one(format!("the policy will not parse as JSON: {e}")))
    }

    /// JSON if it looks like JSON, TOML otherwise — for an import box that
    /// takes either.
    pub fn from_text(text: &str) -> Result<Self, LoadError> {
        if text.trim_start().starts_with('{') {
            Self::from_json(text)
        } else {
            Self::from_toml(text)
        }
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    pub fn rule_mut(&mut self, name: &str) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|rule| rule.name == name)
    }
}

/// Why a policy was not accepted.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub problems: Vec<String>,
}

impl LoadError {
    pub fn one(problem: impl Into<String>) -> Self {
        Self { problems: vec![problem.into()] }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.problems.len() {
            1 => f.write_str(&self.problems[0]),
            _ => {
                writeln!(f, "the policy has {} problems:", self.problems.len())?;
                for problem in &self.problems {
                    writeln!(f, "  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Where a policy came from, for a log line and the admin UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The database — the normal case.
    Database,
    /// A file, being imported or checked. `script_file` in a profile is read
    /// relative to its directory.
    File(PathBuf),
    /// Built in memory: a test, or a document posted to the API.
    Inline,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Database => f.write_str("database"),
            Origin::File(path) => write!(f, "{}", path.display()),
            Origin::Inline => f.write_str("inline"),
        }
    }
}

/// A validated policy, with its rules in evaluation order and every condition
/// compiled.
#[derive(Debug, Clone)]
pub struct RuleSet {
    settings: Settings,
    bootloaders: BTreeMap<String, String>,
    profiles: BTreeMap<String, Profile>,
    rules: Vec<Rule>,
    compiled: Vec<(Compiled, Option<Compiled>)>,
    origin: Origin,
    loaded_at: DateTime<Utc>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            bootloaders: BTreeMap::new(),
            profiles: BTreeMap::new(),
            rules: Vec::new(),
            compiled: Vec::new(),
            origin: Origin::Inline,
            loaded_at: Utc::now(),
        }
    }
}

/// What a name used in a URL and an iPXE menu may contain.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl RuleSet {
    /// A TOML policy, as an earlier release wrote it.
    pub fn parse(toml_text: &str) -> Result<Self, LoadError> {
        Self::from_document(PolicyDocument::from_toml(toml_text)?, Origin::Inline)
    }

    /// A policy file, TOML or JSON by its extension.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            LoadError::one(format!("could not read `{}`: {e}", path.display()))
        })?;
        let document = if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
            PolicyDocument::from_json(&text)
        } else {
            PolicyDocument::from_toml(&text)
        }
        .map_err(|e| LoadError {
            problems: e.problems.into_iter().map(|p| format!("`{}`: {p}", path.display())).collect(),
        })?;
        Self::from_document(document, Origin::File(path.to_path_buf()))
    }

    /// Validate a document and put its rules in evaluation order.
    ///
    /// Everything checkable is checked here, and *all* of it is reported at
    /// once. Handing back the first problem means a five-round-trip fix for a
    /// policy with five mistakes in it.
    pub fn from_document(mut document: PolicyDocument, origin: Origin) -> Result<Self, LoadError> {
        let mut problems = Vec::new();

        // `script_file` is a convenience of a policy kept as files: a long
        // script in its own file next door. Read it in on import, so what the
        // database holds is the whole policy and not a pointer to a file on
        // one machine's disk.
        for (name, profile) in document.profiles.iter_mut() {
            let Some(relative) = profile.script_file.clone() else { continue };
            match &origin {
                Origin::File(path) => {
                    let path = path.parent().unwrap_or(Path::new(".")).join(&relative);
                    match std::fs::read_to_string(&path) {
                        Ok(script) => {
                            profile.script = Some(script);
                            profile.script_file = None;
                        }
                        Err(e) => problems.push(format!(
                            "profile `{name}` names `script_file = \"{relative}\"`, which could \
                             not be read from `{}`: {e}",
                            path.display()
                        )),
                    }
                }
                _ => problems.push(format!(
                    "profile `{name}` names a `script_file`, which is only read when importing a \
                     policy file. Paste the script into `script` instead."
                )),
            }
        }

        for (name, profile) in &document.profiles {
            if !is_plain_name(name) {
                problems.push(format!(
                    "`{name}` is not usable as a profile name: it goes into a URL and an iPXE \
                     menu, so use letters, digits, `-`, `_` and `.` only"
                ));
            }
            problems.extend(profile.problems(name));

            for entry in &profile.entries {
                if !document.profiles.contains_key(&entry.profile) {
                    problems.push(format!(
                        "profile `{name}` has a menu entry for `{}`, which is not a profile",
                        entry.profile
                    ));
                }
            }
            if let Some(default) = &profile.default {
                if !document.profiles.contains_key(default) {
                    problems.push(format!(
                        "profile `{name}` defaults to `{default}`, which is not a profile"
                    ));
                }
            }
        }

        for (arch, file) in &document.bootloaders {
            if arch.trim().is_empty() || file.trim().is_empty() {
                problems.push("a boot loader needs both an architecture and a file".into());
            }
        }

        let mut compiled = Vec::with_capacity(document.rules.len());
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for rule in &document.rules {
            let label = if rule.name.trim().is_empty() { "(unnamed)" } else { rule.name.as_str() };
            if rule.name.trim().is_empty() {
                problems.push("a rule has no name; every rule needs one to appear in a log".into());
            } else if rule.name != rule.name.trim() || rule.name.contains('/') {
                problems.push(format!(
                    "rule `{}` has a name with surrounding spaces or a `/` in it; the name is \
                     used in URLs",
                    rule.name
                ));
            } else if !seen.insert(rule.name.as_str()) {
                problems.push(format!(
                    "two rules are called `{}`; the logs would not say which one fired",
                    rule.name
                ));
            }

            if let Some(profile) = &rule.profile {
                if !document.profiles.contains_key(profile) {
                    problems.push(format!(
                        "rule `{label}` boots `{profile}`, which is not a profile. Declared: {}",
                        list(document.profiles.keys())
                    ));
                }
            }
            if !rule.acts() {
                problems.push(format!(
                    "rule `{label}` neither boots a profile, changes a tag nor sets a variable, \
                     so it does nothing"
                ));
            }
            for tag in rule.tag.iter().chain(&rule.remove_tags) {
                if tag.trim().is_empty() {
                    problems.push(format!("rule `{label}` has an empty tag"));
                }
            }
            for key in rule.set.keys() {
                if !is_plain_name(key) || key.contains('.') {
                    problems.push(format!(
                        "rule `{label}` sets `{key}`, which is not a usable variable name: \
                         letters, digits, `-` and `_` only"
                    ));
                }
            }

            let when = rule.when.compile(&format!("rule `{label}` when"));
            let unless = rule.unless.as_ref().map(|c| c.compile(&format!("rule `{label}` unless")));
            match (when, unless.transpose()) {
                (Ok(when), Ok(unless)) => compiled.push((when, unless)),
                (when, unless) => {
                    problems.extend(when.err().unwrap_or_default());
                    problems.extend(unless.err().unwrap_or_default());
                }
            }
        }

        if let Some(default) = &document.settings.default_profile {
            if !document.profiles.contains_key(default) {
                problems.push(format!(
                    "the default profile is `{default}`, which is not a profile. Declared: {}",
                    list(document.profiles.keys())
                ));
            }
        }

        if document.settings.timezone_offset_minutes.abs() > 14 * 60 {
            problems.push(format!(
                "a timezone offset of {} minutes is more than any timezone on Earth (±14 hours)",
                document.settings.timezone_offset_minutes
            ));
        }

        if !problems.is_empty() {
            return Err(LoadError { problems });
        }

        // Stable sort, so equal priorities keep the order they were written
        // in. A policy with no priorities at all then reads top to bottom,
        // which is what somebody writing one expects.
        let mut order: Vec<usize> = (0..document.rules.len()).collect();
        order.sort_by_key(|&index| std::cmp::Reverse(document.rules[index].priority));

        let mut rules_by_index: Vec<Option<Rule>> = document.rules.into_iter().map(Some).collect();
        let mut compiled_by_index: Vec<Option<(Compiled, Option<Compiled>)>> =
            compiled.into_iter().map(Some).collect();

        let rules = order.iter().filter_map(|&i| rules_by_index[i].take()).collect();
        let compiled = order.iter().filter_map(|&i| compiled_by_index[i].take()).collect();

        Ok(Self {
            settings: document.settings,
            bootloaders: document.bootloaders,
            profiles: document.profiles,
            rules,
            compiled,
            origin,
            loaded_at: Utc::now(),
        })
    }

    /// The policy as a document again: what the API returns, what an export
    /// writes, and what an edit starts from. Rules come out in evaluation
    /// order.
    pub fn to_document(&self) -> PolicyDocument {
        PolicyDocument {
            settings: self.settings.clone(),
            bootloaders: self.bootloaders.clone(),
            profiles: self.profiles.clone(),
            rules: self.rules.clone(),
        }
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn profiles(&self) -> &BTreeMap<String, Profile> {
        &self.profiles
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Where this policy came from.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The same, as text: `database`, a path, or `inline`.
    pub fn source(&self) -> String {
        self.origin.to_string()
    }

    pub fn loaded_at(&self) -> DateTime<Utc> {
        self.loaded_at
    }

    /// The stage-one boot file for an architecture, if the policy named one.
    pub fn bootloader_for(&self, arch_label: &str) -> Option<&str> {
        self.bootloaders
            .get(arch_label)
            .or_else(|| self.bootloaders.get("default"))
            .map(String::as_str)
    }

    pub fn bootloaders(&self) -> &BTreeMap<String, String> {
        &self.bootloaders
    }

    /// One rule's conditions in words — `vendor matches Dell* and known is false`.
    pub fn describe(&self, name: &str) -> Option<(String, Option<String>)> {
        let index = self.rules.iter().position(|rule| rule.name == name)?;
        let (when, unless) = &self.compiled[index];
        Some((when.describe(), unless.as_ref().map(Compiled::describe)))
    }

    /// Run the rules.
    pub fn evaluate(&self, facts: &ClientFacts) -> Evaluation {
        let offset = self.settings.timezone_offset_minutes;
        let mut evaluation = Evaluation::default();

        // The machine's tags as the evaluation goes: what the inventory says,
        // plus and minus what the rules so far have done.
        let mut tags: Vec<String> = facts.tags.clone();
        let mut vars: BTreeMap<String, String> = BTreeMap::new();

        for (rule, (when, unless)) in self.rules.iter().zip(&self.compiled) {
            if !rule.enabled {
                evaluation.trace.push(Trace { rule: rule.name.clone(), outcome: TraceOutcome::Disabled });
                continue;
            }

            let scope = Scope { facts, tags: &tags, vars: &vars, offset_minutes: offset };

            if let Some(miss) = when.mismatch(&scope) {
                evaluation.trace.push(Trace {
                    rule: rule.name.clone(),
                    outcome: TraceOutcome::NoMatch { field: miss.field, detail: miss.detail },
                });
                continue;
            }

            if unless.as_ref().is_some_and(|unless| unless.matches(&scope)) {
                evaluation.trace.push(Trace { rule: rule.name.clone(), outcome: TraceOutcome::Excluded });
                continue;
            }

            for tag in &rule.tag {
                if !contains(&tags, tag) {
                    tags.push(tag.clone());
                }
                if !contains(&evaluation.tags, tag) {
                    evaluation.tags.push(tag.clone());
                }
                evaluation.removed_tags.retain(|held| !held.eq_ignore_ascii_case(tag));
            }
            for tag in &rule.remove_tags {
                tags.retain(|held| !held.eq_ignore_ascii_case(tag));
                evaluation.tags.retain(|held| !held.eq_ignore_ascii_case(tag));
                if !contains(&evaluation.removed_tags, tag) {
                    evaluation.removed_tags.push(tag.clone());
                }
            }
            for (key, value) in &rule.set {
                vars.insert(key.clone(), value.clone());
            }

            let took_effect = evaluation.profile.is_none() && rule.profile.is_some();
            if took_effect {
                evaluation.profile.clone_from(&rule.profile);
                evaluation.chosen_by = Some(rule.name.clone());
            }

            evaluation.matched.push(rule.name.clone());
            evaluation.trace.push(Trace {
                rule: rule.name.clone(),
                outcome: TraceOutcome::Matched {
                    profile: rule.profile.clone(),
                    superseded: rule.profile.is_some() && !took_effect,
                    stopped: rule.stops(),
                },
            });

            if rule.stops() {
                break;
            }
        }

        evaluation.vars = vars;

        if evaluation.profile.is_none() {
            evaluation.profile.clone_from(&self.settings.default_profile);
            evaluation.used_default = evaluation.profile.is_some();
        }

        evaluation
    }
}

fn contains(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|held| held.eq_ignore_ascii_case(tag))
}

fn list<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let names: Vec<&str> = names.map(String::as_str).collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// What one rule did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum TraceOutcome {
    Matched {
        profile: Option<String>,
        /// It named a profile, but an earlier rule had already chosen one.
        superseded: bool,
        stopped: bool,
    },
    /// The first test that did not hold: its fact, and the test in words.
    NoMatch {
        field: String,
        #[serde(default)]
        detail: String,
    },
    /// `when` matched but `unless` did too.
    Excluded,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub rule: String,
    #[serde(flatten)]
    pub outcome: TraceOutcome,
}

/// The result of running a rule set over one machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evaluation {
    pub profile: Option<String>,
    /// Tags the rules added.
    pub tags: Vec<String>,
    /// Tags the rules took away.
    #[serde(default)]
    pub removed_tags: Vec<String>,
    /// Variables the rules set, for the profile's templates.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Rules that fired, in order. A rule that only added tags is in here too,
    /// which is why it is not the same question as `chosen_by`.
    pub matched: Vec<String>,
    /// The rule that actually chose the profile.
    ///
    /// Separate from `matched.first()` on purpose: the first rule to fire is
    /// very often a tagging rule that chose nothing, and crediting it for the
    /// profile sends whoever is reading the log to the wrong rule.
    pub chosen_by: Option<String>,
    /// Whether the profile came from the default rather than a rule.
    pub used_default: bool,
    /// Every rule considered, and what happened to it.
    pub trace: Vec<Trace>,
}

impl Evaluation {
    /// One sentence saying how the decision was reached.
    pub fn reason(&self) -> String {
        match (&self.profile, self.used_default, self.chosen_by.as_deref()) {
            (None, _, _) => "no rule matched and no default profile is declared".to_string(),
            (Some(profile), true, _) => {
                format!("no rule chose a profile, so the default `{profile}` applies")
            }
            (Some(profile), false, Some(chooser)) => {
                let also: Vec<&str> = self
                    .matched
                    .iter()
                    .map(String::as_str)
                    .filter(|name| *name != chooser)
                    .collect();
                if also.is_empty() {
                    format!("rule `{chooser}` chose `{profile}`")
                } else {
                    format!(
                        "rule `{chooser}` chose `{profile}`; also fired: {}",
                        also.join(", ")
                    )
                }
            }
            // A profile with no rule behind it and no default: an override
            // chose it, and `policy::decide` writes its own reason for that.
            (Some(profile), false, None) => format!("`{profile}`"),
        }
    }
}

/// The kind of a named profile, for callers that need to branch on it without
/// reaching into the profile itself.
pub fn kind_of(rules: &RuleSet, profile: &str) -> Option<ProfileKind> {
    rules.profile(profile).map(Profile::kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::oui::OuiDatabase;

    const SAMPLE: &str = r#"
[settings]
default_profile = "menu"

[bootloaders]
bios = "ipxe/undionly.kpxe"
x64-uefi = "ipxe/ipxe.efi"
default = "ipxe/ipxe.efi"

[profiles.local]
label = "Boot from disk"
kind = "local"

[profiles.menu]
kind = "menu"
entries = [{ profile = "local" }, { profile = "ubuntu" }]

[profiles.ubuntu]
label = "Ubuntu 24.04"
kernel = "{{base}}/ubuntu/vmlinuz"

[profiles.winpe]
label = "WinPE"
kernel = "{{base}}/wimboot"

[[rule]]
name = "virtual-machines-never-get-windows"
priority = 100
when = { device_class = ["virtual"] }
profile = "ubuntu"

[[rule]]
name = "tag-the-lab"
when = { subnet = ["10.20.0.0/16"] }
tag = ["lab"]

[[rule]]
name = "dell-workstations"
when = { vendor = ["Dell"], arch = ["x64-uefi"] }
unless = { tag = ["keep"] }
profile = "winpe"
"#;

    fn rules() -> RuleSet {
        RuleSet::parse(SAMPLE).expect("the sample file is valid")
    }

    fn facts(mac: &str, arch: ClientArch) -> ClientFacts {
        ClientFacts::new(mac.parse().unwrap(), arch, Stage::Ipxe)
            .identified(&OuiDatabase::new())
    }

    fn json_rules(document: serde_json::Value) -> RuleSet {
        RuleSet::from_document(serde_json::from_value(document).unwrap(), Origin::Inline).unwrap()
    }

    #[test]
    fn the_sample_file_loads_and_orders_by_priority() {
        let rules = rules();
        assert_eq!(rules.rules()[0].name, "virtual-machines-never-get-windows");
        assert_eq!(rules.profiles().len(), 4);
        assert_eq!(rules.bootloader_for("bios"), Some("ipxe/undionly.kpxe"));
        assert_eq!(rules.bootloader_for("arm64-uefi"), Some("ipxe/ipxe.efi"), "falls back");
    }

    #[test]
    fn a_rule_chooses_a_profile_and_says_which_one_did() {
        let decision = rules().evaluate(&facts("18:66:da:11:22:33", ClientArch::X64_UEFI));
        assert_eq!(decision.profile.as_deref(), Some("winpe"));
        assert_eq!(decision.matched, vec!["dell-workstations"]);
        assert!(decision.reason().contains("dell-workstations"), "{}", decision.reason());
    }

    #[test]
    fn priority_beats_declaration_order() {
        let vm = facts("52:54:00:11:22:33", ClientArch::X64_UEFI);
        let decision = rules().evaluate(&vm);
        assert_eq!(decision.profile.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn unless_carves_an_exception_out_of_a_rule() {
        let mut dell = facts("18:66:da:11:22:33", ClientArch::X64_UEFI);
        dell.tags = vec!["keep".into()];

        let decision = rules().evaluate(&dell);
        assert_eq!(decision.profile.as_deref(), Some("menu"), "falls through to the default");
        assert!(decision.used_default);
        assert!(decision
            .trace
            .iter()
            .any(|t| t.rule == "dell-workstations"
                && matches!(t.outcome, TraceOutcome::Excluded)));
    }

    #[test]
    fn a_tag_only_rule_does_not_stop_evaluation() {
        let mut dell = facts("18:66:da:11:22:33", ClientArch::X64_UEFI);
        dell.client_ip = Some("10.20.1.5".parse().unwrap());

        let decision = rules().evaluate(&dell);
        assert_eq!(decision.tags, vec!["lab"]);
        assert_eq!(decision.profile.as_deref(), Some("winpe"));
        assert_eq!(decision.matched, vec!["tag-the-lab", "dell-workstations"]);
    }

    #[test]
    fn the_reason_credits_the_rule_that_chose_the_profile_not_the_first_to_fire() {
        let mut dell = facts("18:66:da:11:22:33", ClientArch::X64_UEFI);
        dell.client_ip = Some("10.20.1.5".parse().unwrap());

        let decision = rules().evaluate(&dell);
        assert_eq!(decision.matched.first().map(String::as_str), Some("tag-the-lab"));
        assert_eq!(decision.chosen_by.as_deref(), Some("dell-workstations"));
        assert!(
            decision.reason().starts_with("rule `dell-workstations` chose `winpe`"),
            "{}",
            decision.reason()
        );
        assert!(decision.reason().contains("also fired: tag-the-lab"), "{}", decision.reason());
    }

    #[test]
    fn the_trace_names_the_condition_that_failed() {
        let bios_dell = facts("18:66:da:11:22:33", ClientArch::BIOS);
        let decision = rules().evaluate(&bios_dell);

        let trace = decision
            .trace
            .iter()
            .find(|t| t.rule == "dell-workstations")
            .expect("the rule was considered");
        match &trace.outcome {
            TraceOutcome::NoMatch { field, detail } => {
                assert_eq!(field, "arch");
                assert!(detail.contains("x64-uefi"), "{detail}");
            }
            other => panic!("expected a mismatch on the architecture, got {other:?}"),
        }
    }

    #[test]
    fn nothing_matching_falls_back_to_the_declared_default() {
        let stranger = facts("aa:bb:cc:dd:ee:ff", ClientArch::from_code(9000));
        let decision = rules().evaluate(&stranger);
        assert_eq!(decision.profile.as_deref(), Some("menu"));
        assert!(decision.used_default);
        assert!(decision.reason().contains("default"), "{}", decision.reason());
    }

    #[test]
    fn a_rule_naming_a_profile_that_does_not_exist_is_refused_at_load() {
        let error = RuleSet::parse(
            r#"
[profiles.real]
kind = "local"

[[rule]]
name = "typo"
when = { arch = ["bios"] }
profile = "realy"
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("`realy`"), "{error}");
        assert!(error.to_string().contains("real"), "it lists what does exist: {error}");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let error = RuleSet::parse(
            r#"
[profiles.empty]

[[rule]]
name = "a"
profile = "nope"

[[rule]]
name = "a"
tag = ["x"]

[[rule]]
name = "does-nothing"
when = { arch = ["bios"] }
"#,
        )
        .unwrap_err();

        assert!(error.problems.len() >= 4, "{:#?}", error.problems);
        let text = error.to_string();
        assert!(text.contains("nothing for a machine to boot"), "{text}");
        assert!(text.contains("two rules are called"), "{text}");
        assert!(text.contains("does nothing"), "{text}");
    }

    #[test]
    fn a_disabled_rule_is_skipped_and_says_so() {
        let rules = RuleSet::parse(
            r#"
[profiles.a]
kind = "local"
[profiles.b]
kind = "local"

[[rule]]
name = "off"
enabled = false
profile = "a"

[[rule]]
name = "on"
profile = "b"
"#,
        )
        .unwrap();

        let decision = rules.evaluate(&facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS));
        assert_eq!(decision.profile.as_deref(), Some("b"));
        assert!(decision
            .trace
            .iter()
            .any(|t| t.rule == "off" && matches!(t.outcome, TraceOutcome::Disabled)));
    }

    #[test]
    fn an_unknown_key_in_a_legacy_match_is_a_typo_and_is_refused() {
        let error = RuleSet::parse(
            r#"
[profiles.a]
kind = "local"

[[rule]]
name = "typo"
profile = "a"
when = { vendorr = ["Dell"] }
"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("vendorr"), "{error}");
    }

    #[test]
    fn known_distinguishes_a_new_machine_from_a_returning_one() {
        let rules = RuleSet::parse(
            r#"
[profiles.install]
kernel = "x"
[profiles.local]
kind = "local"

[[rule]]
name = "image-new-machines"
when = { known = false }
profile = "install"

[[rule]]
name = "everything-else-boots-itself"
profile = "local"
"#,
        )
        .unwrap();

        let mut new = facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS);
        new.known = false;
        assert_eq!(rules.evaluate(&new).profile.as_deref(), Some("install"));

        let mut returning = new.clone();
        returning.known = true;
        assert_eq!(rules.evaluate(&returning).profile.as_deref(), Some("local"));
    }

    #[test]
    fn a_reimage_window_can_be_restricted_to_the_small_hours() {
        use chrono::TimeZone;
        let rules = RuleSet::parse(
            r#"
[profiles.reimage]
kernel = "x"
[profiles.local]
kind = "local"

[[rule]]
name = "overnight-only"
when = { time_between = ["22:00", "06:00"] }
profile = "reimage"

[[rule]]
name = "daytime"
profile = "local"
"#,
        )
        .unwrap();

        let at = |hour: u32| {
            facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS)
                .at(chrono::Utc.with_ymd_and_hms(2026, 9, 21, hour, 0, 0).unwrap())
        };

        assert_eq!(rules.evaluate(&at(23)).profile.as_deref(), Some("reimage"), "23:00 is inside");
        assert_eq!(rules.evaluate(&at(2)).profile.as_deref(), Some("reimage"), "02:00 wraps");
        assert_eq!(rules.evaluate(&at(12)).profile.as_deref(), Some("local"), "noon is outside");
    }

    #[test]
    fn an_oui_can_be_written_however_the_operator_writes_ouis() {
        let rules = RuleSet::parse(
            r#"
[profiles.a]
kind = "local"

[[rule]]
name = "dell"
profile = "a"
when = { oui = ["18-66-DA", "b083fe"] }
"#,
        )
        .unwrap();

        assert!(!rules.evaluate(&facts("18:66:da:00:00:01", ClientArch::BIOS)).matched.is_empty());
        assert!(!rules.evaluate(&facts("b0:83:fe:00:00:01", ClientArch::BIOS)).matched.is_empty());
        assert!(rules.evaluate(&facts("aa:bb:cc:00:00:01", ClientArch::BIOS)).matched.is_empty());
    }

    #[test]
    fn a_later_rule_cannot_take_the_profile_an_earlier_one_chose() {
        let rules = RuleSet::parse(
            r#"
[profiles.first]
kind = "local"
[profiles.second]
kind = "local"

[[rule]]
name = "chooses"
profile = "first"
stop = false

[[rule]]
name = "also-chooses"
profile = "second"
"#,
        )
        .unwrap();

        let decision = rules.evaluate(&facts("aa:bb:cc:dd:ee:ff", ClientArch::BIOS));
        assert_eq!(decision.profile.as_deref(), Some("first"));
        assert!(decision.trace.iter().any(|t| matches!(
            &t.outcome,
            TraceOutcome::Matched { superseded: true, .. }
        )));
    }

    #[test]
    fn a_rule_can_classify_a_machine_and_a_later_rule_act_on_the_class() {
        // The composition a flat matcher could not express: rule one sets a
        // variable and a tag, rule two reads both.
        let rules = json_rules(serde_json::json!({
            "profiles": { "storage": { "kernel": "{{boot}}/x" }, "local": { "kind": "local" } },
            "rules": [
                { "name": "classify", "priority": 10,
                  "when": { "fact": "vendor", "op": "glob", "value": "Dell*" },
                  "set": { "role": "storage" }, "tag": ["dell"] },
                { "name": "act",
                  "when": { "all": [
                      { "fact": "var", "key": "role", "op": "equals", "value": "storage" },
                      { "fact": "tag", "op": "has_any", "value": ["dell"] }
                  ] },
                  "profile": "storage" },
                { "name": "otherwise", "priority": -1, "profile": "local" }
            ]
        }));

        let decision = rules.evaluate(&facts("18:66:da:11:22:33", ClientArch::X64_UEFI));
        assert_eq!(decision.profile.as_deref(), Some("storage"));
        assert_eq!(decision.vars.get("role").map(String::as_str), Some("storage"));

        let other = rules.evaluate(&facts("aa:bb:cc:11:22:33", ClientArch::X64_UEFI));
        assert_eq!(other.profile.as_deref(), Some("local"));
        assert!(other.vars.is_empty());
    }

    #[test]
    fn a_rule_can_take_a_tag_away() {
        let rules = json_rules(serde_json::json!({
            "rules": [
                { "name": "release-hold",
                  "when": { "fact": "tag", "op": "has_all", "value": ["hold", "released"] },
                  "remove_tags": ["hold", "released"] },
                { "name": "tag-held", "when": { "fact": "tag", "op": "has_any", "value": ["hold"] }, "tag": ["still-held"] }
            ]
        }));

        let mut machine = facts("aa:bb:cc:11:22:33", ClientArch::BIOS);
        machine.tags = vec!["hold".into(), "released".into()];
        let decision = rules.evaluate(&machine);

        assert_eq!(decision.removed_tags, vec!["hold", "released"]);
        assert!(decision.tags.is_empty(), "the later rule saw the tag gone: {:?}", decision.tags);
    }

    #[test]
    fn a_document_round_trips_through_json_and_the_legacy_toml_imports() {
        let original = rules().to_document();
        let json = serde_json::to_string(&original).unwrap();
        let back = PolicyDocument::from_json(&json).unwrap();
        assert_eq!(back, original);
        assert!(json.contains("\"rules\""), "the JSON form says `rules`: {json}");

        // And the TOML export reads back to the same thing.
        let toml = original.to_toml().unwrap();
        assert_eq!(PolicyDocument::from_toml(&toml).unwrap(), original, "{toml}");
    }

    #[test]
    fn a_profile_name_that_cannot_go_in_a_url_is_refused() {
        let error = RuleSet::from_document(
            serde_json::from_value(serde_json::json!({
                "profiles": { "two words": { "kind": "local" } }
            }))
            .unwrap(),
            Origin::Inline,
        )
        .unwrap_err();
        assert!(error.to_string().contains("two words"), "{error}");
    }

    #[test]
    fn a_script_file_is_only_read_from_a_file() {
        let error = RuleSet::from_document(
            serde_json::from_value(serde_json::json!({
                "profiles": { "s": { "script_file": "x.ipxe" } }
            }))
            .unwrap(),
            Origin::Database,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Paste the script"), "{error}");
    }
}
