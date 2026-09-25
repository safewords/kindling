//! The inventory's data access.
//!
//! Every method here is called from the path a machine is waiting on, so the
//! shapes are chosen for that: one query to look a machine up by address, one
//! write when something actually changed, and nothing that reads the whole
//! table to answer a question about one row.
//!
//! Every write is also announced on the [`LiveFeed`], from here rather than
//! from the callers: a sighting over DHCP, a boot counted over HTTP and a pin
//! from the API all land in one of these methods, so this is the one place the
//! admin interface's live view cannot be missed by a new caller.

use std::ops::Deref;

use chrono::{DateTime, Utc};
use rainier_framework::prelude::*;

use crate::app::models::Host;
use crate::app::services::{LiveFeed, LiveUpdate};
use crate::pxe::facts::ClientFacts;
use crate::pxe::mac::MacAddr;

pub struct HostRepository {
    inner: EntityRepository<Host>,
    live: LiveFeed,
}

impl HostRepository {
    pub fn new(database: Database) -> Self {
        Self { inner: EntityRepository::<Host>::new(database), live: LiveFeed::new() }
    }

    /// Announce every change made here on this feed.
    pub fn with_live(mut self, live: LiveFeed) -> Self {
        self.live = live;
        self
    }

    fn announce(&self, host: &Host) {
        self.live.publish(|| LiveUpdate::Host(host.as_json()));
    }

    /// Announce a row changed by a statement that did not hand it back.
    ///
    /// One extra indexed read, and only when somebody is watching — these are
    /// operator actions, not the boot path, so the query is worth a browser
    /// showing the row as it really is rather than as the request asked.
    async fn announce_by_mac(&self, mac: MacAddr) {
        if !self.live.is_watched() {
            return;
        }
        if let Ok(Some(host)) = self.by_mac(mac).await {
            self.announce(&host);
        }
    }

    pub async fn by_mac(&self, mac: MacAddr) -> Result<Option<Host>> {
        self.inner.first_by("mac", mac.to_string().into()).await
    }

    /// Record that a machine was seen, and hand back the row.
    ///
    /// The `known` flag a rule matches on is decided here and nowhere else: it
    /// is false exactly once per machine, on the boot that creates the row.
    /// Deciding it by "was the row created in this call" rather than by a
    /// timestamp comparison is what makes it survive two boots a second apart.
    pub async fn observe(&self, facts: &ClientFacts) -> Result<(Host, bool)> {
        match self.by_mac(facts.mac).await? {
            Some(mut host) => {
                host.observe(facts);
                self.inner.update(&host).await?;
                self.announce(&host);
                Ok((host, true))
            }
            None => {
                let host = self.inner.create(Host::first_sighting(facts)).await?;
                self.announce(&host);
                Ok((host, false))
            }
        }
    }

    /// Save a row the caller changed.
    pub async fn save(&self, host: &Host) -> Result<()> {
        self.inner.update(host).await?;
        self.announce(host);
        Ok(())
    }

    /// Note that a machine booted a profile: the counter, the timestamp and
    /// the last profile, in one write.
    pub async fn record_boot(&self, mac: MacAddr, profile: Option<&str>, at: DateTime<Utc>) -> Result<()> {
        let Some(mut host) = self.by_mac(mac).await? else { return Ok(()) };
        host.boot_count += 1;
        host.last_seen = at;
        host.last_profile = profile.map(str::to_string);
        self.inner.update(&host).await?;
        self.announce(&host);
        Ok(())
    }

    /// Always boot this profile. `None` removes the pin.
    pub async fn pin(&self, mac: MacAddr, profile: Option<&str>) -> Result<bool> {
        self.set_column(mac, "pinned_profile", profile).await
    }

    /// Boot this profile once. `None` removes the one-shot.
    pub async fn set_once(&self, mac: MacAddr, profile: Option<&str>) -> Result<bool> {
        self.set_column(mac, "once_profile", profile).await
    }

    /// Clear a one-shot that has now been used.
    ///
    /// The `where` clause carries the profile that was consumed, so a one-shot
    /// set by an operator *between* the decision and this call is not thrown
    /// away by it. That gap is milliseconds wide and a rack is exactly where
    /// somebody lands in it.
    ///
    /// Not announced on its own: the only caller records the boot straight
    /// afterwards, and that announcement re-reads the row with the one-shot
    /// already gone. Announcing both would cost a query to say the same thing.
    pub async fn consume_once(&self, mac: MacAddr, consumed: &str) -> Result<bool> {
        let cleared = self
            .inner
            .update_column(
                Criteria::new()
                    .where_eq("mac", mac.to_string())
                    .where_eq("once_profile", consumed.to_string()),
                "once_profile",
                None::<String>,
            )
            .await?;
        Ok(cleared == 1)
    }

    async fn set_column(&self, mac: MacAddr, column: &str, value: Option<&str>) -> Result<bool> {
        let updated = self
            .inner
            .update_column(
                Criteria::new().where_eq("mac", mac.to_string()),
                column,
                value.map(str::to_string),
            )
            .await?;
        if updated == 1 {
            self.announce_by_mac(mac).await;
        }
        Ok(updated == 1)
    }

    /// The inventory, newest sighting first, optionally filtered.
    pub async fn page(
        &self,
        page: u64,
        per_page: u64,
        search: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Paginated<Host>> {
        let term = search.map(str::trim).filter(|term| !term.is_empty());

        let criteria = Criteria::new()
            .order_by_desc("last_seen")
            .when(term.is_some(), |criteria| {
                // One typed box over the columns somebody would read off a
                // screen or a sticker.
                let pattern = format!("%{}%", term.unwrap_or_default());
                criteria.or_where(|any| {
                    any.where_like("mac", pattern.clone())
                        .where_like("hostname", pattern.clone())
                        .where_like("vendor", pattern.clone())
                        .where_like("product", pattern.clone())
                        .where_like("serial", pattern.clone())
                })
            })
            // Tags are JSON text, so this is a substring match on the encoded
            // array. Quoting the tag keeps `lab` from matching `lab-retired`.
            .when(tag.is_some(), |criteria| {
                criteria.where_like("tags", format!("%\"{}\"%", tag.unwrap_or_default()))
            });

        self.inner.paginate_matching(criteria, page, per_page).await
    }

    pub async fn all(&self) -> Result<Vec<Host>> {
        self.inner.matching(Criteria::new().order_by_desc("last_seen")).await
    }

    pub async fn count(&self) -> Result<u64> {
        self.inner.count().await
    }

    /// Machines seen since a moment — "what booted today".
    pub async fn seen_since(&self, since: DateTime<Utc>) -> Result<u64> {
        self.inner.count_matching(Criteria::new().where_gte("last_seen", since)).await
    }

    pub async fn with_pins(&self) -> Result<Vec<Host>> {
        self.inner.matching(Criteria::new().where_not_null("pinned_profile")).await
    }

    pub async fn forget(&self, mac: MacAddr) -> Result<bool> {
        let removed =
            self.inner.delete_matching(Criteria::new().where_eq("mac", mac.to_string())).await?;
        if removed == 1 {
            self.live.publish(|| LiveUpdate::HostForgotten { mac: mac.to_string() });
        }
        Ok(removed == 1)
    }
}

impl Deref for HostRepository {
    type Target = EntityRepository<Host>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
