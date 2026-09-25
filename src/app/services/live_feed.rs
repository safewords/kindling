//! What the admin interface is told as it happens.
//!
//! A boot server's screens are watched *while* something is happening — a
//! rack powering on, one machine that will not come up — and a screen that
//! polls is always a few seconds behind the machine it is about. So the places
//! that change something say so here, and every open browser hears it over a
//! WebSocket (`/ws/live`, see `app/http/sockets/live.rs`).
//!
//! Three things about the shape.
//!
//! **It is emitted where the change is recorded**, not where it is requested.
//! The repositories and the rule store publish; the controllers and the three
//! protocols do not. A DHCP offer, a TFTP read, a pin from the API and a tag a
//! rule applied all reach the inventory through one of a handful of methods,
//! so publishing there is the only way the feed cannot miss one — and cannot
//! report a change whose write then failed.
//!
//! **Nothing here can slow a boot down.** Publishing is a non-blocking send
//! into a bounded [`broadcast`] channel. A browser that stops reading falls
//! behind and is told to fetch everything again (a `resync`); it never holds
//! up the machine whose boot produced the message. And with nobody watching,
//! the message is not even built.
//!
//! **It carries what the read API already shows, and nothing else.** Watching
//! needs no token, exactly as `GET /api/hosts` needs none, so a message must
//! never carry what a token guards: not the token, not a configuration value.
//! A configuration change is announced by the *names* of the settings that
//! changed.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::pxe::rules::RuleSet;

/// How many messages a slow browser may fall behind before it is told to
/// resync. A rack of forty machines coming up at once is a few hundred rows
/// in a few seconds; this absorbs that burst for a tab that is briefly busy
/// without holding an unbounded queue for one that has gone to sleep.
const BACKLOG: usize = 1024;

/// One change, as a browser receives it: `{"type": "...", "data": {...}}`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum LiveUpdate {
    /// Sent once, on connect: the connection works, and when it started.
    #[serde(rename = "hello")]
    Hello { at: DateTime<Utc> },

    /// A row in the boot log — the same shape `GET /api/events` returns.
    #[serde(rename = "event")]
    Event(serde_json::Value),

    /// A machine as it now is — the same shape `GET /api/hosts` returns.
    /// Sent whole rather than as a diff, so a browser that missed the
    /// previous message is not left patching a row it never had.
    #[serde(rename = "host")]
    Host(serde_json::Value),

    /// A machine removed from the inventory.
    #[serde(rename = "host.forgotten")]
    HostForgotten { mac: String },

    /// The policy was re-read — from an edit, a reload, or a failed attempt
    /// at either. A failure is sent too: an operator who saved a file that
    /// did not load needs to hear that more than one whose edit worked.
    #[serde(rename = "policy")]
    Policy {
        reloaded: bool,
        rules: Option<usize>,
        profiles: Option<usize>,
        loaded_at: Option<DateTime<Utc>>,
        error: Option<String>,
    },

    /// `.env` was written. Names only — see the module note.
    #[serde(rename = "config")]
    Config { changed: Vec<String> },

    /// This browser fell behind and some messages were dropped for it. Every
    /// screen treats this as "fetch everything again", which is always
    /// correct and only costs what loading the page did.
    #[serde(rename = "resync")]
    Resync { missed: u64 },

    /// The answer to a browser's heartbeat.
    #[serde(rename = "pong")]
    Pong,
}

impl LiveUpdate {
    /// A successful reload, summarised the way `GET /api/health` summarises it.
    pub fn policy_loaded(rules: &RuleSet) -> Self {
        Self::Policy {
            reloaded: true,
            rules: Some(rules.rules().len()),
            profiles: Some(rules.profiles().len()),
            loaded_at: Some(rules.loaded_at()),
            error: None,
        }
    }

    pub fn policy_failed(error: impl Into<String>) -> Self {
        Self::Policy { reloaded: false, rules: None, profiles: None, loaded_at: None, error: Some(error.into()) }
    }

    /// The text frame this becomes.
    ///
    /// Serialised once per message rather than once per socket: forty open
    /// tabs should cost one `to_string`, not forty.
    pub fn to_frame(&self) -> Arc<str> {
        // Every variant is plain data with string keys, so this cannot fail;
        // an empty object is the harmless answer if a future variant can.
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string()).into()
    }
}

/// The channel every change is published on. Cheap to clone; every clone is
/// the same channel.
#[derive(Debug, Clone)]
pub struct LiveFeed {
    sender: broadcast::Sender<Arc<str>>,
}

impl Default for LiveFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveFeed {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BACKLOG);
        Self { sender }
    }

    /// Start listening. Messages published before this call are not seen —
    /// a browser gets the state from the read API and the changes from here.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.sender.subscribe()
    }

    /// How many connections are listening right now.
    pub fn watchers(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Whether anyone would hear a message. Callers with a message that costs
    /// a query to build ask first.
    pub fn is_watched(&self) -> bool {
        self.watchers() > 0
    }

    /// Tell every watcher. The message is only built if someone is watching.
    ///
    /// Never blocks and never fails the caller: this is called from the path a
    /// machine is waiting on, and a browser is never a reason to hold that up.
    pub fn publish(&self, update: impl FnOnce() -> LiveUpdate) {
        if !self.is_watched() {
            return;
        }
        // `Err` here means every receiver went away between the check and the
        // send, which is the same as nobody watching.
        let _ = self.sender.send(update().to_frame());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(frame: &str) -> serde_json::Value {
        serde_json::from_str(frame).expect("a frame is JSON")
    }

    #[test]
    fn every_message_is_a_type_and_its_data() {
        let frame = LiveUpdate::HostForgotten { mac: "18:66:da:11:22:33".into() }.to_frame();
        let value = parse(&frame);
        assert_eq!(value["type"], "host.forgotten");
        assert_eq!(value["data"]["mac"], "18:66:da:11:22:33");

        // A row is passed through as the read API shapes it, not re-wrapped.
        let event = LiveUpdate::Event(serde_json::json!({ "id": 7, "kind": "offer" })).to_frame();
        assert_eq!(parse(&event)["data"]["kind"], "offer");

        // A variant with nothing to say still has a type the browser can
        // switch on.
        assert_eq!(parse(&LiveUpdate::Pong.to_frame())["type"], "pong");
    }

    #[test]
    fn a_failed_reload_is_announced_with_its_reason() {
        let value = parse(&LiveUpdate::policy_failed("line 3: unknown profile").to_frame());
        assert_eq!(value["type"], "policy");
        assert_eq!(value["data"]["reloaded"], false);
        assert_eq!(value["data"]["error"], "line 3: unknown profile");
    }

    #[test]
    fn a_configuration_change_carries_names_and_never_values() {
        // Watching needs no token, so a value — the API token itself, a
        // database password — must never be in a message. The type makes that
        // true: there is nowhere to put one.
        let value = parse(&LiveUpdate::Config { changed: vec!["PXE_API_TOKEN".into()] }.to_frame());
        assert_eq!(value["data"], serde_json::json!({ "changed": ["PXE_API_TOKEN"] }));
    }

    #[tokio::test]
    async fn every_watcher_hears_every_message_in_order() {
        let feed = LiveFeed::new();
        let mut first = feed.subscribe();
        let mut second = feed.clone().subscribe();

        feed.publish(|| LiveUpdate::HostForgotten { mac: "a".into() });
        feed.publish(|| LiveUpdate::HostForgotten { mac: "b".into() });

        for watcher in [&mut first, &mut second] {
            assert_eq!(parse(&watcher.recv().await.unwrap())["data"]["mac"], "a");
            assert_eq!(parse(&watcher.recv().await.unwrap())["data"]["mac"], "b");
        }
    }

    #[test]
    fn with_nobody_watching_a_message_is_never_built() {
        // The boot path publishes on every request; with no browser open that
        // must cost a comparison and nothing more.
        let feed = LiveFeed::new();
        let mut built = false;
        feed.publish(|| {
            built = true;
            LiveUpdate::Pong
        });
        assert!(!built);

        let _watcher = feed.subscribe();
        feed.publish(|| {
            built = true;
            LiveUpdate::Pong
        });
        assert!(built);
    }

    #[test]
    fn a_watcher_that_went_away_stops_counting() {
        let feed = LiveFeed::new();
        let watcher = feed.subscribe();
        assert_eq!(feed.watchers(), 1);
        drop(watcher);
        assert!(!feed.is_watched());
    }

    #[tokio::test]
    async fn a_watcher_that_falls_behind_is_told_how_far_rather_than_blocking_the_sender() {
        let feed = LiveFeed::new();
        let mut slow = feed.subscribe();

        // Nothing here awaits: a full channel drops the oldest message for the
        // slow reader instead of making the publisher wait for it.
        for n in 0..(BACKLOG + 10) {
            feed.publish(|| LiveUpdate::HostForgotten { mac: n.to_string() });
        }

        match slow.recv().await {
            Err(broadcast::error::RecvError::Lagged(missed)) => assert_eq!(missed, 10),
            other => panic!("expected the reader to be told it lagged, got {other:?}"),
        }
    }
}
