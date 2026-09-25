//! The boot policy's data access.
//!
//! Two shapes, for two jobs. The live policy is rows — one per rule, profile,
//! boot loader and setting — and [`sync`](PolicyRepository::sync) writes a
//! whole [`PolicyDocument`] by touching only the rows that differ, so an edit
//! to one rule is one `UPDATE`. The history is documents — one per change —
//! so a rollback is "read that version, sync it", and nothing has to be
//! reconstructed from a trail of diffs.
//!
//! Nothing here validates. That is [`RuleStore`](crate::app::services::RuleStore)'s
//! job, and it does it before anything reaches this module: a document that
//! arrives here has already compiled.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use rainier_framework::prelude::*;
use rainier_orm::Json;

use crate::app::models::{
    PolicyBootloader, PolicyProfile, PolicyRevision, PolicyRule, PolicySetting, RuleTemplate,
};
use crate::pxe::rules::{PolicyDocument, Settings};

/// How many versions of the policy are kept. A busy policy changes a few times
/// a day; this is months of it, and each row is a few kilobytes.
pub const REVISIONS_KEPT: u64 = 500;

const DEFAULT_PROFILE: &str = "default_profile";
const TIMEZONE_OFFSET: &str = "timezone_offset_minutes";

pub struct PolicyRepository {
    rules: EntityRepository<PolicyRule>,
    profiles: EntityRepository<PolicyProfile>,
    bootloaders: EntityRepository<PolicyBootloader>,
    settings: EntityRepository<PolicySetting>,
    revisions: EntityRepository<PolicyRevision>,
    templates: EntityRepository<RuleTemplate>,
}

impl PolicyRepository {
    pub fn new(database: Database) -> Self {
        Self {
            rules: EntityRepository::new(database.clone()),
            profiles: EntityRepository::new(database.clone()),
            bootloaders: EntityRepository::new(database.clone()),
            settings: EntityRepository::new(database.clone()),
            revisions: EntityRepository::new(database.clone()),
            templates: EntityRepository::new(database),
        }
    }

    /// Whether the database has never held a policy — the moment to import
    /// one from a file, or seed the starter.
    ///
    /// A policy that was deliberately emptied still has its history, so it is
    /// not mistaken for a new install and silently re-seeded.
    pub async fn is_blank(&self) -> Result<bool> {
        Ok(self.revisions.count().await? == 0
            && self.rules.count().await? == 0
            && self.profiles.count().await? == 0)
    }

    /// The live policy, as a document. Rules come out in evaluation order.
    pub async fn load(&self) -> Result<PolicyDocument> {
        let rules = self
            .rules
            .matching(Criteria::new().order_by_desc("priority").order_by("position").order_by("id"))
            .await?;
        let profiles = self.profiles.all().await?;
        let bootloaders = self.bootloaders.all().await?;
        let settings = self.settings.all().await?;

        let setting = |key: &str| settings.iter().find(|s| s.key == key).map(|s| s.value.0.clone());

        Ok(PolicyDocument {
            settings: Settings {
                default_profile: setting(DEFAULT_PROFILE)
                    .and_then(|v| v.as_str().map(str::to_string)),
                timezone_offset_minutes: setting(TIMEZONE_OFFSET)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            },
            bootloaders: bootloaders.into_iter().map(|row| (row.arch, row.file)).collect(),
            profiles: profiles.into_iter().map(|row| (row.name, row.body.0)).collect(),
            rules: rules.iter().map(PolicyRule::to_rule).collect(),
        })
    }

    /// Make the rows say what `document` says, writing only what differs.
    ///
    /// There is no transaction to wrap this in, so the order is chosen to be
    /// harmless if it stops half way: new and changed rows first, removals
    /// last. A policy caught mid-sync then has *extra* rows rather than
    /// missing ones — and the running policy never sees it either way, because
    /// the store swaps in the validated document, not a re-read of the rows.
    pub async fn sync(&self, document: &PolicyDocument) -> Result<()> {
        let now = Utc::now();

        // --- profiles, before the rules that point at them ---
        let existing: BTreeMap<String, PolicyProfile> =
            self.profiles.all().await?.into_iter().map(|row| (row.name.clone(), row)).collect();
        for (name, profile) in &document.profiles {
            match existing.get(name) {
                Some(row) if row.body.0 == *profile => {}
                Some(row) => {
                    let mut row = row.clone();
                    row.body = Json(profile.clone());
                    row.updated_at = now;
                    self.profiles.update(&row).await?;
                }
                None => {
                    self.profiles
                        .create(PolicyProfile {
                            id: 0,
                            name: name.clone(),
                            body: Json(profile.clone()),
                            updated_at: now,
                        })
                        .await?;
                }
            }
        }

        // --- rules ---
        let existing: BTreeMap<String, PolicyRule> =
            self.rules.all().await?.into_iter().map(|row| (row.name.clone(), row)).collect();
        for (position, rule) in document.rules.iter().enumerate() {
            let mut wanted = PolicyRule::from_rule(rule, position as i64);
            match existing.get(&rule.name) {
                Some(row) if row.same_as(&wanted) => {}
                Some(row) => {
                    wanted.id = row.id;
                    self.rules.update(&wanted).await?;
                }
                None => {
                    self.rules.create(wanted).await?;
                }
            }
        }

        // --- boot loaders ---
        let existing: BTreeMap<String, PolicyBootloader> =
            self.bootloaders.all().await?.into_iter().map(|row| (row.arch.clone(), row)).collect();
        for (arch, file) in &document.bootloaders {
            match existing.get(arch) {
                Some(row) if row.file == *file => {}
                Some(row) => {
                    let mut row = row.clone();
                    row.file = file.clone();
                    row.updated_at = now;
                    self.bootloaders.update(&row).await?;
                }
                None => {
                    self.bootloaders
                        .create(PolicyBootloader { id: 0, arch: arch.clone(), file: file.clone(), updated_at: now })
                        .await?;
                }
            }
        }

        // --- settings ---
        let wanted_settings = [
            (
                DEFAULT_PROFILE,
                document
                    .settings
                    .default_profile
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            ),
            (TIMEZONE_OFFSET, serde_json::json!(document.settings.timezone_offset_minutes)),
        ];
        let existing: BTreeMap<String, PolicySetting> =
            self.settings.all().await?.into_iter().map(|row| (row.key.clone(), row)).collect();
        for (key, value) in wanted_settings {
            match existing.get(key) {
                Some(row) if row.value.0 == value => {}
                Some(row) => {
                    let mut row = row.clone();
                    row.value = Json(value);
                    row.updated_at = now;
                    self.settings.update(&row).await?;
                }
                None => {
                    self.settings
                        .create(PolicySetting { id: 0, key: key.to_string(), value: Json(value), updated_at: now })
                        .await?;
                }
            }
        }

        // --- removals, last ---
        let keep: BTreeSet<&str> = document.rules.iter().map(|r| r.name.as_str()).collect();
        for row in self.rules.all().await? {
            if !keep.contains(row.name.as_str()) {
                self.rules.delete(row.id.into()).await?;
            }
        }
        for row in self.profiles.all().await? {
            if !document.profiles.contains_key(&row.name) {
                self.profiles.delete(row.id.into()).await?;
            }
        }
        for row in self.bootloaders.all().await? {
            if !document.bootloaders.contains_key(&row.arch) {
                self.bootloaders.delete(row.id.into()).await?;
            }
        }

        Ok(())
    }

    /// Keep this version of the policy, and forget the oldest past the limit.
    pub async fn record(
        &self,
        document: &PolicyDocument,
        summary: &str,
        actor: &str,
    ) -> Result<PolicyRevision> {
        let revision = self
            .revisions
            .create(PolicyRevision {
                id: 0,
                created_at: Utc::now(),
                summary: summary.to_string(),
                actor: actor.to_string(),
                document: Json(document.clone()),
                rules: document.rules.len() as u64,
                profiles: document.profiles.len() as u64,
            })
            .await?;

        if revision.id > REVISIONS_KEPT {
            self.revisions
                .delete_matching(Criteria::new().where_lt("id", revision.id - REVISIONS_KEPT + 1))
                .await?;
        }
        Ok(revision)
    }

    /// The most recent versions, newest first.
    pub async fn revisions(&self, limit: u64) -> Result<Vec<PolicyRevision>> {
        self.revisions.matching(Criteria::new().order_by_desc("id").limit(limit)).await
    }

    pub async fn revision(&self, id: u64) -> Result<Option<PolicyRevision>> {
        self.revisions.find(id.into()).await
    }

    pub async fn latest(&self) -> Result<Option<PolicyRevision>> {
        self.revisions.first_matching(Criteria::new().order_by_desc("id")).await
    }

    // --- the wizard's saved starting points ---

    pub async fn templates(&self) -> Result<Vec<RuleTemplate>> {
        self.templates.matching(Criteria::new().order_by("category").order_by("name")).await
    }

    /// Save a template, replacing one of the same name.
    pub async fn save_template(
        &self,
        name: &str,
        description: Option<String>,
        category: &str,
        rule: serde_json::Value,
    ) -> Result<RuleTemplate> {
        let now = Utc::now();
        match self.templates.first_by("name", name.to_string().into()).await? {
            Some(mut row) => {
                row.description = description;
                row.category = category.to_string();
                row.rule = Json(rule);
                row.updated_at = now;
                self.templates.update(&row).await?;
                Ok(row)
            }
            None => {
                self.templates
                    .create(RuleTemplate {
                        id: 0,
                        name: name.to_string(),
                        description,
                        category: category.to_string(),
                        rule: Json(rule),
                        updated_at: now,
                    })
                    .await
            }
        }
    }

    pub async fn delete_template(&self, id: u64) -> Result<bool> {
        Ok(self.templates.delete(id.into()).await? == 1)
    }
}
