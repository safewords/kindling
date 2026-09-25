//! The boot log's data access.

use std::ops::Deref;

use chrono::{DateTime, Duration, Utc};
use rainier_framework::prelude::*;

use crate::app::models::BootEvent;
use crate::app::services::{LiveFeed, LiveUpdate};
use crate::pxe::mac::MacAddr;

pub struct BootEventRepository {
    inner: EntityRepository<BootEvent>,
    live: LiveFeed,
}

impl BootEventRepository {
    pub fn new(database: Database) -> Self {
        Self { inner: EntityRepository::<BootEvent>::new(database), live: LiveFeed::new() }
    }

    /// Announce every row written here on this feed. Without it the rows are
    /// written and nobody is told, which is what a console command wants.
    pub fn with_live(mut self, live: LiveFeed) -> Self {
        self.live = live;
        self
    }

    /// Write a row, and tell whoever is watching the log.
    ///
    /// Announced *after* the insert, with the row the database handed back, so
    /// a browser is never shown an event that was not kept — and gets the `id`
    /// it needs to tell this row from the ones it already has.
    pub async fn record(&self, event: BootEvent) -> Result<BootEvent> {
        let created = self.inner.create(event).await?;
        self.live.publish(|| LiveUpdate::Event(created.as_json()));
        Ok(created)
    }

    /// The most recent events, newest first.
    pub async fn recent(&self, limit: u64) -> Result<Vec<BootEvent>> {
        self.inner.matching(Criteria::new().order_by_desc("at").order_by_desc("id").limit(limit)).await
    }

    /// One machine's history — the answer to "what happened to this thing".
    pub async fn for_mac(&self, mac: MacAddr, limit: u64) -> Result<Vec<BootEvent>> {
        self.inner
            .matching(
                Criteria::new()
                    .where_eq("mac", mac.to_string())
                    .order_by_desc("at")
                    .order_by_desc("id")
                    .limit(limit),
            )
            .await
    }

    pub async fn since(&self, since: DateTime<Utc>) -> Result<u64> {
        self.inner.count_matching(Criteria::new().where_gte("at", since)).await
    }

    pub async fn count(&self) -> Result<u64> {
        self.inner.count().await
    }

    /// Drop events older than the retention window.
    ///
    /// A boot server on a busy network writes a handful of rows per machine
    /// per boot, and nothing ever reads a row from six months ago. Without
    /// this the table is the only thing in the deployment that grows without
    /// bound.
    pub async fn prune(&self, older_than_days: i64) -> Result<u64> {
        let cutoff = Utc::now() - Duration::days(older_than_days.max(1));
        self.inner.delete_matching(Criteria::new().where_lt("at", cutoff)).await
    }
}

impl Deref for BootEventRepository {
    type Target = EntityRepository<BootEvent>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
