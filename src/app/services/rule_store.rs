//! The boot policy: held in memory, stored in the database, changed in one
//! place.
//!
//! Every decision reads [`current`](RuleStore::current), an `Arc` swapped
//! whole: a boot that started against the old policy finishes against it, and
//! the next one sees the new. Every change — from the web UI's wizard, the
//! API, the console, an import or a rollback — goes through
//! [`apply`](RuleStore::apply), and three things are true of all of them
//! without any caller restating it:
//!
//! - **Nothing that does not validate is ever stored.** The change is made to
//!   a copy of the running document, the copy is compiled by the same loader
//!   the server boots with, and only then does it reach the database.
//! - **Every change is kept.** A [`PolicyRevision`] holds the whole policy as
//!   it stood after each one, so the history is a list and a rollback is a
//!   click.
//! - **Changes are serialised.** Two operators saving at once both land, one
//!   after the other, each on top of the other's — and a caller that says
//!   which revision it was looking at is refused rather than allowed to
//!   silently overwrite a change it never saw.
//!
//! The policy used to be a TOML file. On the first boot against an empty
//! database that file, if it is still there, is imported — so an upgrade keeps
//! its policy — and otherwise the starter policy is seeded.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};

use crate::app::models::PolicyRevision;
use crate::app::repositories::PolicyRepository;
use crate::app::services::{LiveFeed, LiveUpdate};
use crate::pxe::rules::{LoadError, Origin, PolicyDocument, RuleSet};

/// The policy a new install starts with. The same text `pxe:rules --example`
/// prints, so the example can never drift from the default.
pub const STARTER_POLICY: &str = include_str!("../../database/seeds/starter-policy.toml");

/// Why a change was not made.
#[derive(Debug, Clone)]
pub struct EditError {
    pub kind: EditErrorKind,
    /// The problems, in the operator's words. Several at once where possible.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditErrorKind {
    /// The change would leave a policy that does not validate.
    Invalid,
    /// It names a rule, profile or revision that does not exist.
    NotFound,
    /// The policy changed since the caller last looked.
    Conflict,
    /// The database refused. The running policy is unchanged.
    Storage,
}

impl EditError {
    pub fn invalid(problem: impl Into<String>) -> Self {
        Self { kind: EditErrorKind::Invalid, problems: vec![problem.into()] }
    }

    pub fn not_found(problem: impl Into<String>) -> Self {
        Self { kind: EditErrorKind::NotFound, problems: vec![problem.into()] }
    }

    fn storage(problem: impl Into<String>) -> Self {
        Self { kind: EditErrorKind::Storage, problems: vec![problem.into()] }
    }
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problems.join("; "))
    }
}

impl std::error::Error for EditError {}

impl From<LoadError> for EditError {
    fn from(e: LoadError) -> Self {
        Self { kind: EditErrorKind::Invalid, problems: e.problems }
    }
}

/// Who is making a change, and what they would call it.
#[derive(Debug, Clone)]
pub struct EditMeta {
    /// "rule `x` updated" — what the history says.
    pub summary: String,
    /// `web`, `api`, `console`, `import`, `seed`.
    pub actor: String,
    /// The revision the caller was looking at. When given and no longer
    /// current, the change is refused rather than applied over one the caller
    /// never saw.
    pub base_revision: Option<u64>,
}

impl EditMeta {
    pub fn new(summary: impl Into<String>, actor: impl Into<String>) -> Self {
        Self { summary: summary.into(), actor: actor.into(), base_revision: None }
    }

    pub fn based_on(mut self, revision: Option<u64>) -> Self {
        self.base_revision = revision;
        self
    }
}

/// What a successful change produced.
#[derive(Debug, Clone)]
pub struct Applied {
    pub rules: Arc<RuleSet>,
    pub revision: Option<PolicyRevision>,
}

pub struct RuleStore {
    repository: Option<Arc<PolicyRepository>>,
    current: RwLock<Arc<RuleSet>>,
    revision: RwLock<Option<u64>>,
    /// The last failed load, kept so the admin UI can show it. A reload that
    /// failed silently is a policy somebody believes is live.
    last_error: RwLock<Option<(DateTime<Utc>, String)>>,
    /// One change at a time: each is computed from the one before.
    writing: tokio::sync::Mutex<()>,
    live: LiveFeed,
}

impl std::fmt::Debug for RuleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleStore")
            .field("revision", &self.revision())
            .field("rules", &self.current().rules().len())
            .finish()
    }
}

impl RuleStore {
    /// A store backed by the database. Empty until [`load`](Self::load) runs,
    /// which the provider does at boot.
    pub fn database(repository: Arc<PolicyRepository>) -> Self {
        Self {
            repository: Some(repository),
            current: RwLock::new(Arc::new(RuleSet::default().with_origin(Origin::Database))),
            revision: RwLock::new(None),
            last_error: RwLock::new(None),
            writing: tokio::sync::Mutex::new(()),
            live: LiveFeed::new(),
        }
    }

    /// A store with no database behind it, for tests and dry runs.
    pub fn fixed(rules: RuleSet) -> Self {
        Self {
            repository: None,
            current: RwLock::new(Arc::new(rules)),
            revision: RwLock::new(None),
            last_error: RwLock::new(None),
            writing: tokio::sync::Mutex::new(()),
            live: LiveFeed::new(),
        }
    }

    /// Announce every load and every change on this feed.
    pub fn with_live(mut self, live: LiveFeed) -> Self {
        self.live = live;
        self
    }

    /// The policy in force.
    ///
    /// An `Arc` rather than a guard, deliberately: a caller holding a lock
    /// across an `await` would block every other machine's decision on
    /// whatever that caller is waiting for.
    pub fn current(&self) -> Arc<RuleSet> {
        // A panic in a decision must not take the boot server down with it for
        // good. Recovering the value is right here: the data behind the lock
        // is an immutable `Arc` that no partial write can have corrupted.
        Arc::clone(&self.current.read().unwrap_or_else(|p| p.into_inner()))
    }

    /// The revision the running policy is, if it has one.
    pub fn revision(&self) -> Option<u64> {
        *self.revision.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The last load failure, if what is stored is not what is running.
    pub fn last_error(&self) -> Option<(DateTime<Utc>, String)> {
        self.last_error.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn repository(&self) -> Option<&Arc<PolicyRepository>> {
        self.repository.as_ref()
    }

    fn swap(&self, rules: Arc<RuleSet>, revision: Option<u64>) {
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = rules;
        *self.revision.write().unwrap_or_else(|p| p.into_inner()) = revision;
        *self.last_error.write().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn repository_or_refuse(&self) -> Result<&Arc<PolicyRepository>, EditError> {
        self.repository.as_ref().ok_or_else(|| {
            EditError::storage("this policy was supplied directly and has no database to change")
        })
    }

    /// Validate a document without storing it: the same loader the server
    /// runs, so "it validates" and "it would load" are one statement.
    pub fn validate(document: PolicyDocument) -> Result<RuleSet, EditError> {
        Ok(RuleSet::from_document(document, Origin::Inline)?)
    }

    /// Read the policy at boot.
    ///
    /// On an empty database — a new install, or the first boot after
    /// upgrading from the file-based release — a policy is put there first:
    /// the file at `import`, if it exists, otherwise the starter. A stored
    /// policy that does not validate stops the boot: starting with no policy
    /// would mean a rack that boots nothing, which is a worse way to find out
    /// than a message on a terminal.
    pub async fn load(&self, import: Option<&Path>) -> Result<Arc<RuleSet>, LoadError> {
        let repository = self
            .repository
            .as_ref()
            .ok_or_else(|| LoadError::one("this policy has no database to load from"))?;

        let blank = repository.is_blank().await.map_err(storage_problem)?;
        if blank {
            let (document, summary) = match import.filter(|path| path.exists()) {
                Some(path) => {
                    let rules = RuleSet::load(path)?;
                    tracing::warn!(
                        path = %path.display(),
                        rules = rules.rules().len(),
                        profiles = rules.profiles().len(),
                        "imported the policy file into the database. It is no longer read: edit \
                         the policy in the web UI, and delete or archive the file."
                    );
                    (rules.to_document(), format!("imported from `{}`", path.display()))
                }
                None => {
                    tracing::info!("no policy yet; seeding the starter policy");
                    (PolicyDocument::from_toml(STARTER_POLICY)?, "the starter policy".to_string())
                }
            };
            let rules = RuleSet::from_document(document, Origin::Database)?;
            let document = rules.to_document();
            repository.sync(&document).await.map_err(storage_problem)?;
            repository.record(&document, &summary, "seed").await.map_err(storage_problem)?;
        }

        self.reload().await
    }

    /// Re-read the stored policy. On failure the running policy is untouched.
    pub async fn reload(&self) -> Result<Arc<RuleSet>, LoadError> {
        let repository = self
            .repository
            .as_ref()
            .ok_or_else(|| LoadError::one("this policy was supplied directly and has nothing to reload"))?;

        let loaded = async {
            let document = repository.load().await.map_err(storage_problem)?;
            let revision = repository.latest().await.map_err(storage_problem)?.map(|r| r.id);
            let rules = RuleSet::from_document(document, Origin::Database)?;
            Ok::<_, LoadError>((rules, revision))
        }
        .await;

        match loaded {
            Ok((rules, revision)) => {
                let rules = Arc::new(rules);
                self.swap(Arc::clone(&rules), revision);
                tracing::info!(
                    rules = rules.rules().len(),
                    profiles = rules.profiles().len(),
                    revision,
                    "boot policy loaded"
                );
                self.live.publish(|| LiveUpdate::policy_loaded(&rules));
                Ok(rules)
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "the stored policy did not load; the policy already running is unchanged"
                );
                *self.last_error.write().unwrap_or_else(|p| p.into_inner()) =
                    Some((Utc::now(), e.to_string()));
                self.live.publish(|| LiveUpdate::policy_failed(e.to_string()));
                Err(e)
            }
        }
    }

    /// Change the policy.
    ///
    /// `change` edits a copy of the running document. The result is
    /// validated, stored, recorded as a revision, and swapped in — or, on any
    /// failure, none of those, and the running policy is what it was.
    pub async fn apply(
        &self,
        meta: EditMeta,
        change: impl FnOnce(&mut PolicyDocument) -> Result<(), EditError>,
    ) -> Result<Applied, EditError> {
        let repository = self.repository_or_refuse()?;
        let _writing = self.writing.lock().await;

        if let Some(base) = meta.base_revision {
            if Some(base) != self.revision() {
                return Err(EditError {
                    kind: EditErrorKind::Conflict,
                    problems: vec![format!(
                        "the policy changed since you loaded it (you had revision {base}, it is \
                         now {}). Reload to see the other change, then make yours again.",
                        self.revision().map(|r| r.to_string()).unwrap_or_else(|| "none".into())
                    )],
                });
            }
        }

        let before = self.current().to_document();
        let mut document = before.clone();
        change(&mut document)?;

        let rules = RuleSet::from_document(document, Origin::Database)?;
        let document = rules.to_document();

        if document == before {
            // Nothing to store, and a revision saying "nothing changed" would
            // be noise in the one list that should be all signal.
            return Ok(Applied { rules: self.current(), revision: None });
        }

        repository.sync(&document).await.map_err(|e| {
            tracing::error!(error = %e.message(), "the policy change could not be stored");
            EditError::storage(format!(
                "the database refused the change, so nothing changed: {}",
                e.message()
            ))
        })?;

        let revision = match repository.record(&document, &meta.summary, &meta.actor).await {
            Ok(revision) => Some(revision),
            Err(e) => {
                // The change is stored; only its history entry is missing.
                // That is worth a log line, not worth refusing a change that
                // has already happened.
                tracing::warn!(error = %e.message(), "the policy changed but its revision was not recorded");
                None
            }
        };

        let rules = Arc::new(rules);
        self.swap(Arc::clone(&rules), revision.as_ref().map(|r| r.id).or(self.revision()));
        tracing::info!(change = %meta.summary, actor = %meta.actor, "boot policy changed");
        self.live.publish(|| LiveUpdate::policy_loaded(&rules));

        Ok(Applied { rules, revision })
    }

    /// Replace the whole policy.
    pub async fn replace(&self, meta: EditMeta, document: PolicyDocument) -> Result<Applied, EditError> {
        self.apply(meta, move |current| {
            *current = document;
            Ok(())
        })
        .await
    }

    /// Put an earlier version back. The rollback is itself a revision, so it
    /// can be rolled back too.
    pub async fn restore(&self, id: u64, actor: &str) -> Result<Applied, EditError> {
        let repository = self.repository_or_refuse()?;
        let revision = repository
            .revision(id)
            .await
            .map_err(|e| EditError::storage(e.message().to_string()))?
            .ok_or_else(|| EditError::not_found(format!("there is no revision {id}")))?;

        self.replace(
            EditMeta::new(format!("restored revision {id} ({})", revision.summary), actor),
            revision.document.0,
        )
        .await
    }

    /// Import a policy file into the database, replacing what is there.
    pub async fn import(&self, path: &Path, actor: &str) -> Result<Applied, EditError> {
        let rules = RuleSet::load(path)?;
        self.replace(
            EditMeta::new(format!("imported from `{}`", path.display()), actor),
            rules.to_document(),
        )
        .await
    }
}

fn storage_problem(e: rainier_framework::prelude::Error) -> LoadError {
    LoadError::one(format!("the policy could not be read from the database: {}", e.message()))
}

/// Where an earlier release kept its policy file, relative to the working
/// directory, for the one-time import.
pub fn legacy_policy_path(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_policy_is_valid() {
        let rules =
            RuleSet::from_document(PolicyDocument::from_toml(STARTER_POLICY).unwrap(), Origin::Database)
                .expect("the starter policy must load");
        assert!(!rules.rules().is_empty());
        assert!(rules.settings().default_profile.is_some());
    }

    #[tokio::test]
    async fn a_fixed_store_refuses_changes_and_says_why() {
        let store = RuleStore::fixed(RuleSet::default());
        let error = store.apply(EditMeta::new("x", "test"), |_| Ok(())).await.unwrap_err();
        assert_eq!(error.kind, EditErrorKind::Storage);
        assert!(store.reload().await.is_err());
    }
}
