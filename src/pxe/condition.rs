//! Conditions: the question a rule asks about a machine.
//!
//! A condition is a tree. Its leaves are **tests** — one fact, one operator,
//! one value: `product glob ["PowerEdge R6*"]`, `boot_count gte 3`,
//! `network in_subnet ["10.20.0.0/16"]`. Its branches combine them: `all`
//! (every child holds), `any` (at least one does) and `not`.
//!
//! ```json
//! { "all": [
//!     { "fact": "vendor", "op": "glob", "value": ["Dell*"] },
//!     { "any": [
//!         { "fact": "tag", "op": "has_any", "value": ["reimage"] },
//!         { "fact": "known", "op": "is", "value": false }
//!     ] },
//!     { "not": { "fact": "device_class", "op": "in", "value": ["network"] } }
//! ] }
//! ```
//!
//! That is the whole grammar, and it is deliberately the shape the web UI
//! draws: a group is a box, a test is a row. Which facts exist and which
//! operators apply to each is not repeated anywhere else — [`schema`] hands
//! the registry below to the editor, so a fact added here is a fact the editor
//! offers without anybody touching the frontend.
//!
//! Conditions are stored as written and **compiled** when a policy is loaded:
//! globs lowered, regular expressions built, subnets parsed. Everything that
//! can be wrong with a condition is found then, all at once, rather than by a
//! machine at the moment it asks.
//!
//! The older flat form — `{ vendor = ["Dell"], known = false }`, one key per
//! fact, ANDed — is still accepted and converted on the way in, so a policy
//! file written for an earlier release imports without being rewritten.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Datelike, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::facts::{ClientFacts, Stage};
use super::pattern::{Cidr, Glob};
use super::rules::{Match, OuiPrefix, TimeWindow};

// ---------------------------------------------------------------------------
// The stored form.
// ---------------------------------------------------------------------------

/// A condition, as written.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Condition {
    /// Every child holds. An empty `all` holds for every machine, which is
    /// how a catch-all rule is written.
    All { all: Vec<Condition> },
    /// At least one child holds. An empty `any` holds for none.
    Any { any: Vec<Condition> },
    /// The child does not hold.
    Not { not: Box<Condition> },
    /// One fact, one operator, one value.
    Test(Test),
}

/// A leaf: `fact op value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    pub fact: String,
    /// For facts that are a family rather than a single value — `var`, whose
    /// key names the variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub op: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
}

impl Default for Condition {
    fn default() -> Self {
        Condition::always()
    }
}

impl Condition {
    /// Matches every machine.
    pub fn always() -> Self {
        Condition::All { all: Vec::new() }
    }

    pub fn test(fact: &str, op: &str, value: Value) -> Self {
        Condition::Test(Test { fact: fact.to_string(), key: None, op: op.to_string(), value })
    }

    /// Whether this holds for every machine without asking anything.
    pub fn is_always(&self) -> bool {
        matches!(self, Condition::All { all } if all.iter().all(Condition::is_always))
    }

    /// The facts this condition asks about, in the order it asks, each once.
    pub fn facts(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_facts(&mut out);
        out
    }

    fn collect_facts(&self, out: &mut Vec<String>) {
        match self {
            Condition::All { all: children } | Condition::Any { any: children } => {
                children.iter().for_each(|child| child.collect_facts(out))
            }
            Condition::Not { not } => not.collect_facts(out),
            Condition::Test(test) => {
                let name = match &test.key {
                    Some(key) => format!("{}.{key}", test.fact),
                    None => test.fact.clone(),
                };
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }

    /// Read a condition out of JSON, accepting the flat legacy form too.
    ///
    /// Written by hand rather than derived because the derived error for an
    /// untagged enum is "data did not match any variant", which tells somebody
    /// who typed `vendorr` nothing at all.
    pub fn from_json(value: Value) -> Result<Self, String> {
        match value {
            Value::Array(items) => Ok(Condition::All {
                all: items.into_iter().map(Condition::from_json).collect::<Result<_, _>>()?,
            }),
            Value::Object(mut map) => {
                for group in ["all", "any"] {
                    if let Some(children) = map.remove(group) {
                        if !map.is_empty() {
                            return Err(format!(
                                "a group has `{group}` and also {}; a group holds only its children",
                                keys(&map)
                            ));
                        }
                        let Value::Array(children) = children else {
                            return Err(format!("`{group}` must be a list of conditions"));
                        };
                        let children = children
                            .into_iter()
                            .map(Condition::from_json)
                            .collect::<Result<Vec<_>, _>>()?;
                        return Ok(if group == "all" {
                            Condition::All { all: children }
                        } else {
                            Condition::Any { any: children }
                        });
                    }
                }
                if let Some(inner) = map.remove("not") {
                    if !map.is_empty() {
                        return Err(format!(
                            "`not` holds one condition and nothing beside it, but it also has {}",
                            keys(&map)
                        ));
                    }
                    return Ok(Condition::Not { not: Box::new(Condition::from_json(inner)?) });
                }
                if map.contains_key("fact") {
                    return serde_json::from_value::<Test>(Value::Object(map))
                        .map(Condition::Test)
                        .map_err(|e| format!("a test will not parse: {e}"));
                }
                // The flat form: `{ vendor = ["Dell"], known = false }`.
                let legacy: Match = serde_json::from_value(Value::Object(map))
                    .map_err(|e| format!("a condition will not parse: {e}"))?;
                Ok(legacy.into_condition())
            }
            other => Err(format!(
                "a condition is a group (`all`, `any`, `not`) or a test (`fact`, `op`, `value`), \
                 not `{other}`"
            )),
        }
    }
}

fn keys(map: &serde_json::Map<String, Value>) -> String {
    map.keys().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Condition::from_json(value).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// The registry: which facts exist, and what can be asked of each.
// ---------------------------------------------------------------------------

/// What kind of value a fact holds, which decides the operators it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    Text,
    /// Text drawn from a closed list.
    Choice,
    Number,
    Bool,
    Address,
    Tags,
    Time,
    Weekday,
    Arch,
    Oui,
}

impl FactType {
    /// The operators this type takes, in the order an editor should offer
    /// them — the everyday one first.
    pub fn operators(self) -> &'static [&'static str] {
        match self {
            FactType::Text => &[
                "glob",
                "equals",
                "not_equals",
                "in",
                "not_in",
                "not_glob",
                "contains",
                "starts_with",
                "ends_with",
                "regex",
                "exists",
                "missing",
            ],
            FactType::Choice => &["in", "not_in", "equals", "not_equals"],
            FactType::Number => &["gte", "lte", "eq", "ne", "gt", "lt", "between"],
            FactType::Bool => &["is"],
            FactType::Address => &["in_subnet", "not_in_subnet", "equals", "exists", "missing"],
            FactType::Tags => &["has_any", "has_all", "has_none", "empty", "not_empty"],
            FactType::Time => &["between", "not_between"],
            FactType::Weekday => &["in", "not_in"],
            FactType::Arch => &["in", "not_in"],
            FactType::Oui => &["in", "not_in"],
        }
    }
}

/// One fact a rule can ask about.
#[derive(Debug, Clone, Serialize)]
pub struct FactSpec {
    pub name: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub ty: FactType,
    /// What it is and when it is known. The editor shows this beside the row.
    pub help: &'static str,
    /// For a `choice` or `weekday` fact, the values it can take.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub choices: &'static [&'static str],
    /// What to put in an empty value box.
    pub example: &'static str,
    /// Whether the fact needs a `key` — `var` does.
    pub keyed: bool,
    /// `firmware`, `ipxe` or `always`: when in a boot this fact is known. A
    /// rule on an iPXE-only fact never fires at the DHCP stage, and saying so
    /// in the editor is cheaper than finding out from a rack.
    pub known_at: &'static str,
    /// Grouping for the editor's fact picker.
    pub group: &'static str,
}

const DEVICE_CLASSES: &[&str] = &["physical", "virtual", "sbc", "network", "unknown"];
const STAGES: &[&str] = &["firmware", "ipxe"];
const IPXE_BUILDS: &[&str] = &["not-ipxe", "unidentified", "own"];
const WEEKDAYS: &[&str] =
    &["mon", "tue", "wed", "thu", "fri", "sat", "sun", "weekday", "weekend"];

macro_rules! fact {
    ($name:expr, $label:expr, $ty:ident, $group:expr, $known:expr, $example:expr, $help:expr) => {
        FactSpec {
            name: $name,
            label: $label,
            ty: FactType::$ty,
            help: $help,
            choices: &[],
            example: $example,
            keyed: false,
            known_at: $known,
            group: $group,
        }
    };
}

/// Every fact. The editor's list, the validator's list and the evaluator's
/// list are this one.
pub const FACTS: &[FactSpec] = &[
    // --- identity ---
    fact!("mac", "MAC address", Text, "Identity", "always", "18:66:da:*",
        "The hardware address, lowercase and colon-separated."),
    fact!("oui", "OUI (vendor prefix)", Oui, "Identity", "always", "18:66:da",
        "The first three octets of the address — the part IEEE assigns to a manufacturer. Any spelling works: 18:66:da, 1866DA, 18-66-da."),
    fact!("vendor", "Vendor", Text, "Identity", "always", "Dell*",
        "Resolved from the OUI, or from SMBIOS when the OUI is unknown."),
    FactSpec {
        choices: DEVICE_CLASSES,
        ..fact!("device_class", "Device class", Choice, "Identity", "always", "physical",
            "Inferred from the address. `network` is switches and access points — the ones that must never be handed an installer.")
    },
    fact!("hostname", "Hostname", Text, "Identity", "always", "lab-*", "DHCP option 12, when the machine sends one."),
    fact!("uuid", "SMBIOS UUID", Text, "Identity", "always", "4c4c4544-*", "DHCP option 97."),
    // --- hardware (SMBIOS, iPXE only) ---
    fact!("manufacturer", "Manufacturer", Text, "Hardware", "ipxe", "Dell Inc.",
        "From SMBIOS, so only known once iPXE is running — a rule on this never fires at the DHCP stage."),
    fact!("product", "Product / model", Text, "Hardware", "ipxe", "PowerEdge R6*",
        "From SMBIOS. Use a glob: model numbers come with suffixes."),
    fact!("serial", "Serial number", Text, "Hardware", "ipxe", "7XK2M13", "From SMBIOS."),
    fact!("asset", "Asset tag", Text, "Hardware", "ipxe", "IT-00421", "From SMBIOS."),
    fact!("platform", "Firmware platform", Text, "Hardware", "ipxe", "efi", "iPXE's `${platform}`: `pcbios` or `efi`."),
    // --- boot ---
    fact!("arch", "Architecture", Arch, "Boot", "always", "x64-uefi",
        "A label (`x64-uefi`), a family (`arm64`), a firmware (`uefi`, `bios`), `http`/`tftp`, or the raw option 93 number."),
    FactSpec {
        choices: STAGES,
        ..fact!("stage", "Boot stage", Choice, "Boot", "always", "ipxe",
            "Which half of the boot is asking: `firmware` is the DHCP exchange, `ipxe` is the script request.")
    },
    fact!("ipxe", "iPXE is asking", Bool, "Boot", "always", "true",
        "True once the machine is running iPXE rather than its own firmware."),
    FactSpec {
        choices: IPXE_BUILDS,
        ..fact!("ipxe_build", "iPXE build", Choice, "Boot", "always", "own",
            "Whose iPXE: `own` is this project's build, `unidentified` any other.")
    },
    fact!("http_boot", "HTTP boot", Bool, "Boot", "always", "true", "The firmware asked over HTTP rather than TFTP."),
    fact!("vendor_class", "Vendor class (opt. 60)", Text, "Boot", "always", "PXEClient:Arch:00007*", "DHCP option 60, as the firmware sends it."),
    fact!("user_class", "User class (opt. 77)", Text, "Boot", "always", "iPXE*", "DHCP option 77. iPXE puts its own name here."),
    // --- network ---
    fact!("network", "Network", Address, "Network", "always", "10.20.0.0/16",
        "The relay's address if the request was relayed, otherwise the client's own — the address that says which network a machine is on."),
    fact!("client_ip", "Client address", Address, "Network", "always", "10.20.1.0/24", "The machine's own address, once it has one."),
    fact!("relay_ip", "Relay address", Address, "Network", "firmware", "10.20.0.1", "The DHCP relay (giaddr) the request came through."),
    // --- inventory ---
    fact!("tag", "Tags", Tags, "Inventory", "always", "hold",
        "Labels on the machine in the inventory, plus any an earlier rule in this evaluation added."),
    fact!("known", "Has booted here before", Bool, "Inventory", "always", "false",
        "`false` is how 'image anything new' is written. It is the boot counter, not the existence of a row, so it stays false for the whole of a first boot."),
    fact!("boot_count", "Boots so far", Number, "Inventory", "always", "3", "How many times this machine has been handed a script here."),
    // --- time ---
    fact!("time", "Time of day", Time, "Time", "always", "22:00-06:00",
        "A window of the day, in UTC shifted by the policy's timezone offset. Wraps midnight, so 22:00–06:00 is one window."),
    FactSpec {
        choices: WEEKDAYS,
        ..fact!("weekday", "Day of the week", Weekday, "Time", "always", "weekend",
            "`mon`…`sun`, or `weekday` / `weekend`.")
    },
    // --- rule variables ---
    FactSpec {
        keyed: true,
        ..fact!("var", "Variable", Text, "Variables", "always", "server",
            "A variable an earlier rule set with its `set` action. How one rule classifies a machine and a later one acts on the class.")
    },
];

pub fn fact(name: &str) -> Option<&'static FactSpec> {
    FACTS.iter().find(|spec| spec.name == name)
}

/// Every operator, with what it means — for the editor's labels and help.
pub const OPERATORS: &[(&str, &str, &str)] = &[
    // (name, label, what the value is)
    ("glob", "matches", "one or more patterns; `*` is any run, `?` one character"),
    ("not_glob", "does not match", "one or more patterns"),
    ("equals", "is", "a value (case-insensitive)"),
    ("not_equals", "is not", "a value (case-insensitive)"),
    ("in", "is one of", "a list of values"),
    ("not_in", "is none of", "a list of values"),
    ("contains", "contains", "one or more fragments"),
    ("starts_with", "starts with", "one or more prefixes"),
    ("ends_with", "ends with", "one or more suffixes"),
    ("regex", "matches the regex", "one or more regular expressions (case-insensitive)"),
    ("exists", "is known", "nothing"),
    ("missing", "is unknown", "nothing"),
    ("eq", "=", "a whole number"),
    ("ne", "≠", "a whole number"),
    ("gt", ">", "a whole number"),
    ("gte", "≥", "a whole number"),
    ("lt", "<", "a whole number"),
    ("lte", "≤", "a whole number"),
    ("between", "is between", "two bounds, inclusive"),
    ("not_between", "is outside", "two bounds"),
    ("is", "is", "yes or no"),
    ("in_subnet", "is in", "one or more networks in CIDR form"),
    ("not_in_subnet", "is not in", "one or more networks in CIDR form"),
    ("has_any", "include any of", "one or more tags"),
    ("has_all", "include all of", "one or more tags"),
    ("has_none", "include none of", "one or more tags"),
    ("empty", "are empty", "nothing"),
    ("not_empty", "are not empty", "nothing"),
];

/// The registry, for the web UI.
pub fn schema() -> Value {
    let operators: BTreeMap<&str, Value> = OPERATORS
        .iter()
        .map(|(name, label, value)| {
            (*name, serde_json::json!({ "label": label, "value": value, "takes_value": takes_value(name) }))
        })
        .collect();

    serde_json::json!({
        "facts": FACTS.iter().map(|spec| {
            let mut json = serde_json::to_value(spec).unwrap_or_default();
            json["operators"] = serde_json::json!(spec.ty.operators());
            json
        }).collect::<Vec<_>>(),
        "operators": operators,
    })
}

fn takes_value(op: &str) -> bool {
    !matches!(op, "exists" | "missing" | "empty" | "not_empty")
}

// ---------------------------------------------------------------------------
// Compilation.
// ---------------------------------------------------------------------------

/// A condition ready to run: every pattern parsed, every fact resolved.
#[derive(Debug, Clone)]
pub struct Compiled(Node);

#[derive(Debug, Clone)]
enum Node {
    All(Vec<Node>),
    Any(Vec<Node>),
    Not(Box<Node>, String),
    Leaf(Leaf),
}

#[derive(Debug, Clone)]
struct Leaf {
    fact: &'static FactSpec,
    key: Option<String>,
    matcher: Matcher,
    /// `vendor matches Dell*`, for a trace.
    described: String,
}

#[derive(Debug, Clone)]
enum Matcher {
    Equals(Vec<String>),
    NotEquals(Vec<String>),
    Glob(Vec<Glob>),
    NotGlob(Vec<Glob>),
    Contains(Vec<String>),
    StartsWith(Vec<String>),
    EndsWith(Vec<String>),
    Regex(Vec<Regex>),
    Exists,
    Missing,
    Compare(std::cmp::Ordering, bool, i64),
    NotEqualNumber(i64),
    Between(i64, i64, bool),
    Is(bool),
    InSubnet(Vec<Cidr>, bool),
    AddressEquals(Vec<std::net::IpAddr>),
    HasAny(Vec<String>),
    HasAll(Vec<String>),
    HasNone(Vec<String>),
    Empty(bool),
    Window(TimeWindow, bool),
    Weekday(Vec<String>, bool),
    Arch(Vec<String>, bool),
    Oui(Vec<OuiPrefix>, bool),
}

impl Condition {
    /// Compile, or say everything that is wrong with it. `at` names where the
    /// condition lives — `rule \`x\` when` — so a problem says where to look.
    pub fn compile(&self, at: &str) -> Result<Compiled, Vec<String>> {
        let mut problems = Vec::new();
        let node = compile_node(self, at, &mut problems);
        if problems.is_empty() {
            Ok(Compiled(node))
        } else {
            Err(problems)
        }
    }
}

fn compile_node(condition: &Condition, at: &str, problems: &mut Vec<String>) -> Node {
    match condition {
        Condition::All { all } => {
            Node::All(all.iter().map(|child| compile_node(child, at, problems)).collect())
        }
        Condition::Any { any } => {
            if any.is_empty() {
                problems.push(format!(
                    "{at}: an empty `any` group can never hold, so this never matches. Add a \
                     condition to it or remove it."
                ));
            }
            Node::Any(any.iter().map(|child| compile_node(child, at, problems)).collect())
        }
        Condition::Not { not } => {
            let inner = compile_node(not, at, problems);
            let described = describe_node(&inner);
            Node::Not(Box::new(inner), described)
        }
        Condition::Test(test) => match compile_test(test) {
            Ok(leaf) => Node::Leaf(leaf),
            Err(problem) => {
                problems.push(format!("{at}: {problem}"));
                // A placeholder that never matches; it is never run, because
                // the policy that contains it is refused.
                Node::Any(Vec::new())
            }
        },
    }
}

fn describe_node(node: &Node) -> String {
    match node {
        Node::Leaf(leaf) => leaf.described.clone(),
        Node::All(children) if children.is_empty() => "anything".into(),
        Node::All(children) => {
            children.iter().map(describe_node).collect::<Vec<_>>().join(" and ")
        }
        Node::Any(children) => format!(
            "({})",
            children.iter().map(describe_node).collect::<Vec<_>>().join(" or ")
        ),
        Node::Not(_, described) => format!("not ({described})"),
    }
}

fn compile_test(test: &Test) -> Result<Leaf, String> {
    let spec = fact(&test.fact).ok_or_else(|| {
        format!(
            "`{}` is not a fact a rule can ask about. Known: {}",
            test.fact,
            FACTS.iter().map(|f| f.name).collect::<Vec<_>>().join(", ")
        )
    })?;

    if spec.keyed && test.key.as_deref().map(str::trim).unwrap_or_default().is_empty() {
        return Err(format!("`{}` needs a `key` naming which one", spec.name));
    }
    if !spec.keyed && test.key.is_some() {
        return Err(format!("`{}` takes no `key`", spec.name));
    }

    let op = test.op.as_str();
    if !spec.ty.operators().contains(&op) {
        return Err(format!(
            "`{}` cannot be asked `{op}`. It takes: {}",
            spec.name,
            spec.ty.operators().join(", ")
        ));
    }

    let value = &test.value;
    let name = spec.name;
    let strings = || strings(value, name, op);

    let matcher = match (spec.ty, op) {
        (_, "exists") => Matcher::Exists,
        (_, "missing") => Matcher::Missing,
        (FactType::Tags, "empty") => Matcher::Empty(true),
        (FactType::Tags, "not_empty") => Matcher::Empty(false),

        (FactType::Text | FactType::Choice, "equals" | "in") => {
            Matcher::Equals(check_choices(spec, lowered(strings()?))?)
        }
        (FactType::Text | FactType::Choice, "not_equals" | "not_in") => {
            Matcher::NotEquals(check_choices(spec, lowered(strings()?))?)
        }
        (FactType::Text, "glob") => Matcher::Glob(strings()?.into_iter().map(Glob::new).collect()),
        (FactType::Text, "not_glob") => {
            Matcher::NotGlob(strings()?.into_iter().map(Glob::new).collect())
        }
        (FactType::Text, "contains") => Matcher::Contains(lowered(strings()?)),
        (FactType::Text, "starts_with") => Matcher::StartsWith(lowered(strings()?)),
        (FactType::Text, "ends_with") => Matcher::EndsWith(lowered(strings()?)),
        (FactType::Text, "regex") => Matcher::Regex(
            strings()?
                .into_iter()
                .map(|pattern| {
                    regex::RegexBuilder::new(&pattern)
                        .case_insensitive(true)
                        .size_limit(1 << 20)
                        .build()
                        .map_err(|e| format!("`{pattern}` is not a regular expression: {e}"))
                })
                .collect::<Result<_, _>>()?,
        ),

        (FactType::Number, "eq") => Matcher::Compare(std::cmp::Ordering::Equal, false, number(value, name)?),
        (FactType::Number, "ne") => Matcher::NotEqualNumber(number(value, name)?),
        (FactType::Number, "gt") => Matcher::Compare(std::cmp::Ordering::Greater, false, number(value, name)?),
        (FactType::Number, "gte") => Matcher::Compare(std::cmp::Ordering::Greater, true, number(value, name)?),
        (FactType::Number, "lt") => Matcher::Compare(std::cmp::Ordering::Less, false, number(value, name)?),
        (FactType::Number, "lte") => Matcher::Compare(std::cmp::Ordering::Less, true, number(value, name)?),
        (FactType::Number, "between") => {
            let (low, high) = pair(value, name)?;
            let low = parse_number(&low, name)?;
            let high = parse_number(&high, name)?;
            if low > high {
                return Err(format!("`{name} between` has its bounds the wrong way round: {low} > {high}"));
            }
            Matcher::Between(low, high, true)
        }

        (FactType::Bool, "is") => match value {
            Value::Bool(b) => Matcher::Is(*b),
            Value::String(s) if matches!(s.as_str(), "true" | "yes") => Matcher::Is(true),
            Value::String(s) if matches!(s.as_str(), "false" | "no") => Matcher::Is(false),
            other => return Err(format!("`{name} is` takes true or false, not `{other}`")),
        },

        (FactType::Address, "in_subnet" | "not_in_subnet") => Matcher::InSubnet(
            strings()?
                .into_iter()
                .map(|text| {
                    // A bare address is a /32 (or /128): what somebody means
                    // when they paste one in.
                    let text = if text.contains('/') {
                        text
                    } else if text.contains(':') {
                        format!("{text}/128")
                    } else {
                        format!("{text}/32")
                    };
                    text.parse::<Cidr>().map_err(|e| e.to_string())
                })
                .collect::<Result<_, _>>()?,
            op == "in_subnet",
        ),
        (FactType::Address, "equals") => Matcher::AddressEquals(
            strings()?
                .into_iter()
                .map(|text| {
                    text.parse().map_err(|_| format!("`{text}` is not an IP address"))
                })
                .collect::<Result<_, _>>()?,
        ),

        (FactType::Tags, "has_any") => Matcher::HasAny(strings()?),
        (FactType::Tags, "has_all") => Matcher::HasAll(strings()?),
        (FactType::Tags, "has_none") => Matcher::HasNone(strings()?),

        (FactType::Time, "between" | "not_between") => {
            let (from, to) = match value {
                Value::String(text) => text
                    .split_once(['-', '–'])
                    .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                    .ok_or_else(|| format!("`{text}` is not a window; write it as `22:00-06:00`"))?,
                other => pair(other, name)?,
            };
            let window: TimeWindow = serde_json::from_value(serde_json::json!([from, to]))
                .map_err(|e| e.to_string())?;
            Matcher::Window(window, op == "between")
        }

        (FactType::Weekday, "in" | "not_in") => {
            let days = lowered(strings()?);
            for day in &days {
                if !WEEKDAYS.contains(&day.as_str())
                    && !["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "weekdays", "weekends"]
                        .contains(&day.as_str())
                {
                    return Err(format!("`{day}` is not a day; use {}", WEEKDAYS.join(", ")));
                }
            }
            Matcher::Weekday(days, op == "in")
        }

        (FactType::Arch, "in" | "not_in") => Matcher::Arch(strings()?, op == "in"),
        (FactType::Oui, "in" | "not_in") => Matcher::Oui(
            strings()?.iter().map(|s| s.parse()).collect::<Result<_, _>>()?,
            op == "in",
        ),

        (ty, op) => return Err(format!("`{op}` is not implemented for {ty:?} facts")),
    };

    let label = match &test.key {
        Some(key) => format!("{}.{key}", spec.name),
        None => spec.name.to_string(),
    };
    let op_label = OPERATORS.iter().find(|(n, ..)| *n == op).map(|(_, l, _)| *l).unwrap_or(op);
    let described = if takes_value(op) {
        format!("{label} {op_label} {}", show_value(value))
    } else {
        format!("{label} {op_label}")
    };

    Ok(Leaf { fact: spec, key: test.key.clone(), matcher, described })
}

fn show_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(show_value).collect::<Vec<_>>().join(" | "),
        other => other.to_string(),
    }
}

/// A value that may be one string or a list of them, as a list.
fn strings(value: &Value, fact: &str, op: &str) -> Result<Vec<String>, String> {
    let items: Vec<String> = match value {
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.clone()),
                Value::Number(n) => Ok(n.to_string()),
                other => Err(format!("`{fact} {op}` takes text, not `{other}`")),
            })
            .collect::<Result<_, _>>()?,
        Value::Null => Vec::new(),
        other => return Err(format!("`{fact} {op}` takes text or a list of it, not `{other}`")),
    };
    let items: Vec<String> =
        items.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if items.is_empty() {
        return Err(format!("`{fact} {op}` has no value to compare against"));
    }
    Ok(items)
}

fn lowered(items: Vec<String>) -> Vec<String> {
    items.into_iter().map(|s| s.to_lowercase()).collect()
}

fn check_choices(spec: &FactSpec, values: Vec<String>) -> Result<Vec<String>, String> {
    if spec.ty == FactType::Choice {
        for value in &values {
            if !spec.choices.contains(&value.as_str()) {
                return Err(format!(
                    "`{value}` is not a {}; it is one of {}",
                    spec.name,
                    spec.choices.join(", ")
                ));
            }
        }
    }
    Ok(values)
}

fn number(value: &Value, fact: &str) -> Result<i64, String> {
    match value {
        Value::Number(n) => n.as_i64().ok_or_else(|| format!("`{fact}` compares whole numbers, not `{n}`")),
        Value::String(s) => parse_number(s, fact),
        other => Err(format!("`{fact}` compares whole numbers, not `{other}`")),
    }
}

fn parse_number(text: &str, fact: &str) -> Result<i64, String> {
    text.trim().parse().map_err(|_| format!("`{fact}` compares whole numbers, not `{text}`"))
}

fn pair(value: &Value, fact: &str) -> Result<(String, String), String> {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match value {
        Value::Array(items) if items.len() == 2 => Ok((text(&items[0]), text(&items[1]))),
        Value::Object(map) if map.contains_key("from") && map.contains_key("to") => {
            Ok((text(&map["from"]), text(&map["to"])))
        }
        other => Err(format!("`{fact}` takes two bounds, `[from, to]`, not `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// Evaluation.
// ---------------------------------------------------------------------------

/// What a condition is evaluated against: the machine, plus what earlier rules
/// in the same evaluation have said about it.
pub struct Scope<'a> {
    pub facts: &'a ClientFacts,
    /// The machine's tags as they stand at this point in the evaluation.
    pub tags: &'a [String],
    pub vars: &'a BTreeMap<String, String>,
    pub offset_minutes: i64,
}

/// Why a condition did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// The fact whose test failed — the short answer, e.g. `arch`.
    pub field: String,
    /// The whole test, e.g. `arch is one of x64-uefi`.
    pub detail: String,
}

impl Compiled {
    pub fn matches(&self, scope: &Scope<'_>) -> bool {
        self.mismatch(scope).is_none()
    }

    /// `None` when the condition holds; otherwise the **first** test that
    /// kept it from holding. The name is the point: "did not match" is useless
    /// to somebody staring at a machine that booted the wrong thing.
    pub fn mismatch(&self, scope: &Scope<'_>) -> Option<Mismatch> {
        node_mismatch(&self.0, scope)
    }

    pub fn describe(&self) -> String {
        describe_node(&self.0)
    }
}

fn node_mismatch(node: &Node, scope: &Scope<'_>) -> Option<Mismatch> {
    match node {
        Node::All(children) => children.iter().find_map(|child| node_mismatch(child, scope)),
        Node::Any(children) => {
            let mut first = None;
            for child in children {
                match node_mismatch(child, scope) {
                    None => return None,
                    Some(miss) => {
                        first.get_or_insert(miss);
                    }
                }
            }
            Some(match first {
                Some(miss) if children.len() == 1 => miss,
                Some(miss) => Mismatch { field: miss.field, detail: format!("none of {}", describe_node(node)) },
                None => Mismatch { field: "any".into(), detail: "an empty `any` group".into() },
            })
        }
        Node::Not(inner, described) => match node_mismatch(inner, scope) {
            Some(_) => None,
            None => Some(Mismatch {
                field: format!("not {}", first_field(inner)),
                detail: format!("not ({described})"),
            }),
        },
        Node::Leaf(leaf) => {
            if leaf_holds(leaf, scope) {
                None
            } else {
                Some(Mismatch {
                    field: match &leaf.key {
                        Some(key) => format!("{}.{key}", leaf.fact.name),
                        None => leaf.fact.name.to_string(),
                    },
                    detail: leaf.described.clone(),
                })
            }
        }
    }
}

fn first_field(node: &Node) -> String {
    match node {
        Node::Leaf(leaf) => leaf.fact.name.to_string(),
        Node::All(children) | Node::Any(children) => {
            children.first().map(first_field).unwrap_or_else(|| "group".into())
        }
        Node::Not(inner, _) => first_field(inner),
    }
}

/// The value of a text-like fact for this machine.
fn text_fact(name: &str, key: Option<&str>, scope: &Scope<'_>) -> Option<String> {
    let facts = scope.facts;
    match name {
        "mac" => Some(facts.mac.to_string()),
        "vendor" => facts.vendor.clone(),
        "device_class" => Some(facts.device_class.as_str().to_string()),
        "hostname" => facts.hostname.clone(),
        "uuid" => facts.uuid.clone(),
        "manufacturer" => facts.manufacturer.clone(),
        "product" => facts.product.clone(),
        "serial" => facts.serial.clone(),
        "asset" => facts.asset.clone(),
        "platform" => facts.platform.clone(),
        "stage" => Some(facts.stage.as_str().to_string()),
        "ipxe_build" => Some(facts.ipxe_build().as_str().to_string()),
        "vendor_class" => facts.vendor_class.clone(),
        "user_class" => facts.user_class.clone(),
        "var" => key.and_then(|key| scope.vars.get(key).cloned()),
        "arch" => Some(facts.arch.label()),
        "oui" => Some(facts.oui.clone()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn address_fact(name: &str, scope: &Scope<'_>) -> Option<std::net::IpAddr> {
    match name {
        "network" => scope.facts.network_address(),
        "client_ip" => scope.facts.client_ip,
        "relay_ip" => scope.facts.relay_ip.map(std::net::IpAddr::V4),
        _ => None,
    }
}

fn bool_fact(name: &str, scope: &Scope<'_>) -> bool {
    match name {
        "ipxe" => scope.facts.is_ipxe(),
        "http_boot" => scope.facts.is_http_boot(),
        "known" => scope.facts.known,
        _ => false,
    }
}

fn has_tag(scope: &Scope<'_>, tag: &str) -> bool {
    scope.tags.iter().any(|held| held.eq_ignore_ascii_case(tag))
}

fn leaf_holds(leaf: &Leaf, scope: &Scope<'_>) -> bool {
    let name = leaf.fact.name;
    let key = leaf.key.as_deref();
    let text = || text_fact(name, key, scope);

    match &leaf.matcher {
        Matcher::Exists => match leaf.fact.ty {
            FactType::Address => address_fact(name, scope).is_some(),
            _ => text().is_some(),
        },
        Matcher::Missing => match leaf.fact.ty {
            FactType::Address => address_fact(name, scope).is_none(),
            _ => text().is_none(),
        },
        Matcher::Equals(values) => {
            text().is_some_and(|t| values.iter().any(|v| t.eq_ignore_ascii_case(v)))
        }
        // A fact that is not known is not equal to anything, so "is not" holds.
        Matcher::NotEquals(values) => {
            !text().is_some_and(|t| values.iter().any(|v| t.eq_ignore_ascii_case(v)))
        }
        Matcher::Glob(globs) => {
            let value = text();
            globs.iter().any(|g| g.matches_option(value.as_deref()))
        }
        Matcher::NotGlob(globs) => {
            let value = text();
            !globs.iter().any(|g| g.matches_option(value.as_deref()))
        }
        Matcher::Contains(parts) => {
            text().is_some_and(|t| parts.iter().any(|p| t.to_lowercase().contains(p.as_str())))
        }
        Matcher::StartsWith(parts) => {
            text().is_some_and(|t| parts.iter().any(|p| t.to_lowercase().starts_with(p.as_str())))
        }
        Matcher::EndsWith(parts) => {
            text().is_some_and(|t| parts.iter().any(|p| t.to_lowercase().ends_with(p.as_str())))
        }
        Matcher::Regex(patterns) => text().is_some_and(|t| patterns.iter().any(|r| r.is_match(&t))),

        Matcher::Compare(ordering, or_equal, bound) => {
            let actual = scope.facts.boot_count as i64;
            actual.cmp(bound) == *ordering || (*or_equal && actual == *bound)
        }
        Matcher::NotEqualNumber(bound) => scope.facts.boot_count as i64 != *bound,
        Matcher::Between(low, high, inside) => {
            let actual = scope.facts.boot_count as i64;
            (actual >= *low && actual <= *high) == *inside
        }

        Matcher::Is(wanted) => bool_fact(name, scope) == *wanted,

        Matcher::InSubnet(nets, inside) => match address_fact(name, scope) {
            Some(address) => nets.iter().any(|net| net.contains(address)) == *inside,
            // An unknown address is in no subnet: `in` fails, `not in` holds.
            None => !*inside,
        },
        Matcher::AddressEquals(addresses) => {
            address_fact(name, scope).is_some_and(|address| addresses.contains(&address))
        }

        Matcher::HasAny(tags) => tags.iter().any(|tag| has_tag(scope, tag)),
        Matcher::HasAll(tags) => tags.iter().all(|tag| has_tag(scope, tag)),
        Matcher::HasNone(tags) => !tags.iter().any(|tag| has_tag(scope, tag)),
        Matcher::Empty(wanted) => scope.tags.is_empty() == *wanted,

        Matcher::Window(window, inside) => {
            window.contains(scope.facts.at, scope.offset_minutes) == *inside
        }
        Matcher::Weekday(days, inside) => {
            weekday_matches(days, scope.facts.at, scope.offset_minutes) == *inside
        }
        Matcher::Arch(names, inside) => {
            names.iter().any(|n| scope.facts.arch.matches_name(n)) == *inside
        }
        Matcher::Oui(prefixes, inside) => {
            prefixes.iter().any(|p| p.matches(scope.facts.mac)) == *inside
        }
    }
}

fn weekday_matches(wanted: &[String], at: DateTime<Utc>, offset_minutes: i64) -> bool {
    let shifted = at + Duration::minutes(offset_minutes);
    let number = shifted.weekday().num_days_from_monday();
    let short = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"][number as usize];
    let long =
        ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"][number as usize];

    wanted.iter().any(|name| match name.as_str() {
        "weekday" | "weekdays" => number < 5,
        "weekend" | "weekends" => number >= 5,
        other => other == short || other == long,
    })
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.compile("") {
            Ok(compiled) => f.write_str(&compiled.describe()),
            Err(_) => f.write_str(&serde_json::to_string(self).unwrap_or_default()),
        }
    }
}

// ---------------------------------------------------------------------------
// The legacy flat form.
// ---------------------------------------------------------------------------

impl Match {
    /// The flat form as a tree: each key a test, all of them ANDed, in the
    /// order the old matcher checked them — so the first mismatch a trace
    /// reports is the one it always reported.
    pub fn into_condition(self) -> Condition {
        let mut all = Vec::new();
        let list = |items: Vec<String>| Value::Array(items.into_iter().map(Value::String).collect());
        let globs = |items: &[Glob]| list(items.iter().map(|g| g.as_str().to_string()).collect());

        if !self.mac.is_empty() {
            all.push(Condition::test("mac", "glob", globs(&self.mac)));
        }
        if !self.oui.is_empty() {
            all.push(Condition::test("oui", "in", list(self.oui.iter().map(|o| o.to_string()).collect())));
        }
        for (name, items) in [("vendor", &self.vendor)] {
            if !items.is_empty() {
                all.push(Condition::test(name, "glob", globs(items)));
            }
        }
        if !self.device_class.is_empty() {
            all.push(Condition::test("device_class", "in", list(lowered(self.device_class.clone()))));
        }
        if !self.arch.is_empty() {
            all.push(Condition::test("arch", "in", list(self.arch.clone())));
        }
        for (name, items) in [
            ("vendor_class", &self.vendor_class),
            ("user_class", &self.user_class),
            ("hostname", &self.hostname),
            ("uuid", &self.uuid),
            ("manufacturer", &self.manufacturer),
            ("product", &self.product),
            ("serial", &self.serial),
            ("asset", &self.asset),
        ] {
            if !items.is_empty() {
                all.push(Condition::test(name, "glob", globs(items)));
            }
        }
        if !self.subnet.is_empty() {
            all.push(Condition::test(
                "network",
                "in_subnet",
                list(self.subnet.iter().map(|c| c.to_string()).collect()),
            ));
        }
        if !self.tag.is_empty() {
            all.push(Condition::test("tag", "has_any", list(self.tag.clone())));
        }
        if let Some(stage) = self.stage {
            all.push(Condition::test("stage", "equals", Value::String(stage_name(stage).into())));
        }
        if let Some(ipxe) = self.ipxe {
            all.push(Condition::test("ipxe", "is", Value::Bool(ipxe)));
        }
        if let Some(known) = self.known {
            all.push(Condition::test("known", "is", Value::Bool(known)));
        }
        if let Some(least) = self.boots_at_least {
            all.push(Condition::test("boot_count", "gte", Value::from(least)));
        }
        if let Some(most) = self.boots_at_most {
            all.push(Condition::test("boot_count", "lte", Value::from(most)));
        }
        if let Some(window) = self.time_between {
            all.push(Condition::test("time", "between", serde_json::to_value(window).unwrap_or_default()));
        }
        if !self.weekday.is_empty() {
            all.push(Condition::test("weekday", "in", list(self.weekday.clone())));
        }

        Condition::All { all }
    }
}

fn stage_name(stage: Stage) -> &'static str {
    stage.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::oui::OuiDatabase;
    use serde_json::json;

    fn facts() -> ClientFacts {
        ClientFacts::new("18:66:da:11:22:33".parse().unwrap(), ClientArch::X64_UEFI, Stage::Ipxe)
            .with_smbios(Some("Dell Inc.".into()), Some("PowerEdge R630".into()), None, None)
            .with_client_ip(Some("10.20.1.5".parse().unwrap()))
            .identified(&OuiDatabase::new())
    }

    fn holds(condition: Value, facts: &ClientFacts) -> bool {
        holds_with(condition, facts, &[], &BTreeMap::new())
    }

    fn holds_with(
        condition: Value,
        facts: &ClientFacts,
        tags: &[String],
        vars: &BTreeMap<String, String>,
    ) -> bool {
        let condition = Condition::from_json(condition).expect("parses");
        let compiled = condition.compile("test").expect("compiles");
        compiled.matches(&Scope { facts, tags, vars, offset_minutes: 0 })
    }

    #[test]
    fn a_tree_of_groups_and_tests_evaluates_as_written() {
        let facts = facts();
        assert!(holds(
            json!({ "all": [
                { "fact": "vendor", "op": "glob", "value": ["Dell*"] },
                { "any": [
                    { "fact": "product", "op": "glob", "value": "PowerEdge R6*" },
                    { "fact": "product", "op": "equals", "value": "OptiPlex 7090" }
                ] },
                { "not": { "fact": "device_class", "op": "in", "value": ["network"] } }
            ] }),
            &facts
        ));
        assert!(!holds(
            json!({ "not": { "fact": "product", "op": "regex", "value": "^poweredge r\\d{3}$" } }),
            &facts
        ));
    }

    #[test]
    fn every_operator_family_does_what_it_says() {
        let facts = facts();
        let cases = [
            (json!({ "fact": "network", "op": "in_subnet", "value": ["10.20.0.0/16"] }), true),
            (json!({ "fact": "network", "op": "not_in_subnet", "value": "10.20.0.0/16" }), false),
            (json!({ "fact": "client_ip", "op": "equals", "value": "10.20.1.5" }), true),
            (json!({ "fact": "boot_count", "op": "lte", "value": 0 }), true),
            (json!({ "fact": "boot_count", "op": "between", "value": [1, 3] }), false),
            (json!({ "fact": "hostname", "op": "missing" }), true),
            (json!({ "fact": "product", "op": "contains", "value": "r63" }), true),
            (json!({ "fact": "product", "op": "starts_with", "value": ["optiplex", "poweredge"] }), true),
            (json!({ "fact": "oui", "op": "in", "value": ["18-66-DA"] }), true),
            (json!({ "fact": "arch", "op": "not_in", "value": ["bios"] }), true),
            (json!({ "fact": "known", "op": "is", "value": false }), true),
            (json!({ "fact": "tag", "op": "empty" }), true),
        ];
        for (condition, expected) in cases {
            assert_eq!(holds(condition.clone(), &facts), expected, "{condition}");
        }
    }

    #[test]
    fn tags_and_variables_come_from_the_scope_not_only_the_machine() {
        // What lets one rule classify a machine and a later one act on it.
        let facts = facts();
        let tags = vec!["lab".to_string()];
        let mut vars = BTreeMap::new();
        vars.insert("role".to_string(), "storage".to_string());

        assert!(holds_with(json!({ "fact": "tag", "op": "has_all", "value": ["LAB"] }), &facts, &tags, &vars));
        assert!(holds_with(
            json!({ "fact": "var", "key": "role", "op": "equals", "value": "storage" }),
            &facts,
            &tags,
            &vars
        ));
        assert!(!holds_with(
            json!({ "fact": "var", "key": "rack", "op": "exists" }),
            &facts,
            &tags,
            &vars
        ));
    }

    #[test]
    fn every_problem_in_a_condition_is_reported_with_where_it_is() {
        let condition = Condition::from_json(json!({ "all": [
            { "fact": "vendorr", "op": "glob", "value": "Dell" },
            { "fact": "boot_count", "op": "glob", "value": "3" },
            { "fact": "product", "op": "regex", "value": "(" },
            { "fact": "network", "op": "in_subnet", "value": "10.0.0.0/33" },
            { "fact": "device_class", "op": "in", "value": ["virtaul"] },
            { "fact": "var", "op": "exists" },
            { "any": [] }
        ] }))
        .unwrap();

        let problems = condition.compile("rule `x` when").unwrap_err();
        assert_eq!(problems.len(), 7, "{problems:#?}");
        assert!(problems.iter().all(|p| p.starts_with("rule `x` when")), "{problems:#?}");
        let text = problems.join("\n");
        for needle in ["vendorr", "cannot be asked `glob`", "not a regular expression", "virtaul", "needs a `key`", "empty `any`"] {
            assert!(text.contains(needle), "missing {needle}: {text}");
        }
    }

    #[test]
    fn a_misspelt_key_is_refused_rather_than_matching_everything() {
        let error = Condition::from_json(json!({ "vendorr": ["Dell"] })).unwrap_err();
        assert!(error.contains("vendorr"), "{error}");
        let error = Condition::from_json(json!({ "fact": "vendor", "op": "glob", "valeu": "x" })).unwrap_err();
        assert!(error.contains("valeu"), "{error}");
    }

    #[test]
    fn the_flat_legacy_form_becomes_an_equivalent_tree() {
        let condition = Condition::from_json(json!({ "vendor": ["Dell"], "known": false, "subnet": ["10.20.0.0/16"] })).unwrap();
        assert_eq!(condition.facts(), vec!["vendor", "network", "known"]);
        assert!(holds(serde_json::to_value(&condition).unwrap(), &facts()));
    }

    #[test]
    fn a_trace_names_the_first_test_that_failed() {
        let condition = Condition::from_json(json!({ "all": [
            { "fact": "vendor", "op": "glob", "value": "Dell" },
            { "fact": "arch", "op": "in", "value": ["bios"] }
        ] }))
        .unwrap();
        let compiled = condition.compile("t").unwrap();
        let facts = facts();
        let miss = compiled
            .mismatch(&Scope { facts: &facts, tags: &[], vars: &BTreeMap::new(), offset_minutes: 0 })
            .unwrap();
        assert_eq!(miss.field, "arch");
        assert!(miss.detail.contains("bios"), "{}", miss.detail);
    }

    #[test]
    fn the_schema_offers_only_operators_the_compiler_accepts() {
        // The editor is built from the schema, so an operator listed there
        // that the compiler refuses would be a form that cannot be saved.
        for spec in FACTS {
            for op in spec.ty.operators() {
                let value = match (spec.ty, *op) {
                    (_, "between" | "not_between") if spec.ty == FactType::Time => json!(["22:00", "06:00"]),
                    (_, "between") => json!([1, 2]),
                    (FactType::Number, _) => json!(1),
                    (FactType::Bool, _) => json!(true),
                    (FactType::Address, "equals") => json!("10.0.0.1"),
                    (FactType::Address, _) => json!("10.0.0.0/8"),
                    (FactType::Choice | FactType::Weekday, _) => json!([spec.choices[0]]),
                    (FactType::Oui, _) => json!("18:66:da"),
                    _ => json!("x"),
                };
                let test = Test {
                    fact: spec.name.into(),
                    key: spec.keyed.then(|| "k".to_string()),
                    op: (*op).into(),
                    value: if takes_value(op) { value } else { Value::Null },
                };
                Condition::Test(test.clone())
                    .compile("schema")
                    .unwrap_or_else(|e| panic!("{} {op}: {e:?}", spec.name));
                assert!(OPERATORS.iter().any(|(n, ..)| n == op), "`{op}` has no label");
            }
        }
    }

    #[test]
    fn a_condition_round_trips_through_json() {
        let original = json!({ "any": [
            { "fact": "tag", "op": "has_any", "value": ["hold"] },
            { "not": { "fact": "var", "key": "role", "op": "equals", "value": "db" } },
            { "all": [] }
        ] });
        let condition = Condition::from_json(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&condition).unwrap(), original);
    }
}
