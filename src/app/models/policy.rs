//! The boot policy, as rows.
//!
//! One table per kind of thing an operator edits — rules, profiles, boot
//! loaders, settings — so an edit to one rule is a write to one row, and a
//! query can answer "which rules boot `ubuntu-2404`" without loading the rest.
//!
//! The parts of a rule and a profile that are themselves structured — a
//! condition tree, a list of initrds, a menu — are JSON columns. They are only
//! ever read and written whole, and a table per condition node would be a
//! schema that exists to be joined back together.
//!
//! Beside them, [`PolicyRevision`] keeps every version of the whole policy as
//! one document: the history the admin UI lists, and what a rollback restores.
//! That is the part of keeping a policy in version control worth keeping, now
//! that it is not in a file.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rainier_framework::prelude::*;
use rainier_orm::Json;

use crate::pxe::condition::Condition;
use crate::pxe::profile::Profile;
use crate::pxe::rules::{PolicyDocument, Rule};

#[derive(Entity, Clone, Debug)]
#[orm(table = "policy_rules")]
pub struct PolicyRule {
    #[orm(pk, auto_increment)]
    pub id: u64,
    /// What the boot log says fired. Unique, or the log would be ambiguous.
    #[orm(unique)]
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    /// Order among rules of equal priority.
    pub position: i64,
    pub when_condition: Json<Condition>,
    pub unless_condition: Json<Option<Condition>>,
    pub profile: Option<String>,
    pub add_tags: Json<Vec<String>>,
    pub remove_tags: Json<Vec<String>>,
    pub set_vars: Json<BTreeMap<String, String>>,
    pub stop: Option<bool>,
    pub updated_at: DateTime<Utc>,
}

impl Model for PolicyRule {
    fn route_key_name() -> &'static str {
        "name"
    }
}

impl PolicyRule {
    pub fn from_rule(rule: &Rule, position: i64) -> Self {
        Self {
            id: 0,
            name: rule.name.clone(),
            description: rule.description.clone(),
            enabled: rule.enabled,
            priority: rule.priority,
            position,
            when_condition: Json(rule.when.clone()),
            unless_condition: Json(rule.unless.clone()),
            profile: rule.profile.clone(),
            add_tags: Json(rule.tag.clone()),
            remove_tags: Json(rule.remove_tags.clone()),
            set_vars: Json(rule.set.clone()),
            stop: rule.stop,
            updated_at: Utc::now(),
        }
    }

    pub fn to_rule(&self) -> Rule {
        Rule {
            name: self.name.clone(),
            description: self.description.clone(),
            enabled: self.enabled,
            priority: self.priority,
            when: self.when_condition.0.clone(),
            unless: self.unless_condition.0.clone(),
            profile: self.profile.clone(),
            tag: self.add_tags.0.clone(),
            remove_tags: self.remove_tags.0.clone(),
            set: self.set_vars.0.clone(),
            stop: self.stop,
        }
    }

    /// Whether writing `other` over this row would change anything.
    pub fn same_as(&self, other: &PolicyRule) -> bool {
        self.to_rule() == other.to_rule() && self.position == other.position
    }
}

#[derive(Entity, Clone, Debug)]
#[orm(table = "policy_profiles")]
pub struct PolicyProfile {
    #[orm(pk, auto_increment)]
    pub id: u64,
    #[orm(unique)]
    pub name: String,
    /// The profile's fields — kind, kernel, initrds, menu entries and the
    /// rest — as one document, because which of them exist depends on the
    /// kind.
    pub body: Json<Profile>,
    pub updated_at: DateTime<Utc>,
}

impl Model for PolicyProfile {
    fn route_key_name() -> &'static str {
        "name"
    }
}

#[derive(Entity, Clone, Debug)]
#[orm(table = "policy_bootloaders")]
pub struct PolicyBootloader {
    #[orm(pk, auto_increment)]
    pub id: u64,
    /// An architecture label, or `default`.
    #[orm(unique)]
    pub arch: String,
    /// A path inside the boot root, or a URL.
    pub file: String,
    pub updated_at: DateTime<Utc>,
}

impl Model for PolicyBootloader {}

/// One policy-wide setting. A key and a JSON value rather than a column per
/// setting, so adding one is not a migration.
#[derive(Entity, Clone, Debug)]
#[orm(table = "policy_settings")]
pub struct PolicySetting {
    #[orm(pk, auto_increment)]
    pub id: u64,
    #[orm(unique)]
    pub key: String,
    pub value: Json<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

impl Model for PolicySetting {}

/// A version of the whole policy, kept after every change.
#[derive(Entity, Clone, Debug)]
#[orm(table = "policy_revisions")]
#[orm(index = "created_at")]
pub struct PolicyRevision {
    #[orm(pk, auto_increment)]
    pub id: u64,
    pub created_at: DateTime<Utc>,
    /// What changed, in words: "rule `x` updated".
    pub summary: String,
    /// Who or what made the change: `web`, `api`, `console`, `import`, `seed`.
    pub actor: String,
    /// The policy as it stood *after* this change.
    pub document: Json<PolicyDocument>,
    pub rules: u64,
    pub profiles: u64,
}

impl Model for PolicyRevision {}

impl PolicyRevision {
    /// The listing shape: everything but the document.
    pub fn as_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "created_at": self.created_at,
            "summary": self.summary,
            "actor": self.actor,
            "rules": self.rules,
            "profiles": self.profiles,
        })
    }
}

/// A starting point for the rule wizard, saved by an operator.
///
/// Deliberately not part of the policy: a template decides nothing about any
/// machine, so it is not validated against the profiles, not versioned with
/// the rules, and deleting one changes no boot. It is a partial rule — any of
/// the rule's fields — that the wizard's first step offers beside its
/// built-in starters.
#[derive(Entity, Clone, Debug)]
#[orm(table = "rule_templates")]
pub struct RuleTemplate {
    #[orm(pk, auto_increment)]
    pub id: u64,
    #[orm(unique)]
    pub name: String,
    pub description: Option<String>,
    /// Which step of the wizard the template is about, for grouping:
    /// `who`, `what`, `when`, or anything else an operator types.
    pub category: String,
    pub rule: Json<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

impl Model for RuleTemplate {}

impl RuleTemplate {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "category": self.category,
            "rule": self.rule.0,
            "updated_at": self.updated_at,
        })
    }
}
