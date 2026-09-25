//! `/ws/live` — every change, pushed to every open admin screen.
//!
//! Served by the framework's own WebSocket support on the HTTP port rather
//! than on a listener of its own. An upgrade is a `GET` the same accept loop
//! already takes, so there is no second port to open in a firewall, no second
//! address for the browser to guess, and no second thing that can be half
//! started — which on a boot server, where every listener failing looks the
//! same, is worth more than it would be elsewhere.
//!
//! **Open, like the read API.** Everything sent here is something
//! `GET /api/hosts` or `GET /api/events` would hand to anyone who asked, so
//! requiring the token to *watch* would protect nothing and would make the
//! live view the one screen that breaks without it. That is also why no
//! origin check: a page on another site that opened this socket would learn
//! what it could already `fetch`. What keeps that true is
//! [`LiveUpdate`](crate::app::services::LiveUpdate) — it has no variant that
//! can carry a secret.
//!
//! **One way.** The browser sends only a heartbeat. Everything that changes
//! anything stays on the HTTP API, behind the token, where it already is.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rainier_framework::prelude::*;
use rainier_framework::websocket::{Message, SocketId};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::AbortHandle;

use crate::app::services::{LiveFeed, LiveUpdate};

/// How long a browser may say nothing before it is presumed gone.
///
/// The browser pings every 25 seconds. The reason this exists at all is that
/// a socket's outgoing queue is unbounded: a laptop that went to sleep holding
/// a half-open connection would otherwise have every boot event on the network
/// queued for it until TCP gave up, which can be hours. This caps that at a
/// minute and a half of messages.
const SILENCE: Duration = Duration::from_secs(90);

/// How often the silence is checked. Coarse on purpose: this is reclaiming
/// memory from a connection that is already dead, not racing anything.
const CHECK_EVERY: Duration = Duration::from_secs(15);

pub struct LiveSocket {
    feed: LiveFeed,
    /// One forwarding task per open connection, so `on_close` can stop it.
    /// Without that the task would outlive its socket until the next message
    /// happened to fail to send — which on a quiet network is never.
    connections: Mutex<HashMap<SocketId, Connection>>,
}

struct Connection {
    forwarder: AbortHandle,
    /// When the browser was last heard from, in Unix seconds.
    heard: Arc<AtomicI64>,
}

impl LiveSocket {
    pub fn new(feed: LiveFeed) -> Self {
        Self { feed, connections: Mutex::new(HashMap::new()) }
    }

    /// How many browsers are connected — for a test, and for anyone curious.
    pub fn connections(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SocketId, Connection>> {
        // The map holds handles, not state a panic could leave half-written.
        self.connections.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl WebSocketHandler for LiveSocket {
    async fn on_connect(&self, socket: &Socket) -> Result<()> {
        // Subscribed before the greeting goes out, so a change that happens
        // while the browser is reading the greeting is not lost between them.
        let mut changes = self.feed.subscribe();
        socket.send(LiveUpdate::Hello { at: Utc::now() }.to_frame().as_ref())?;

        let heard = Arc::new(AtomicI64::new(Utc::now().timestamp()));
        let outbound = socket.clone();
        let last_heard = Arc::clone(&heard);

        let forwarder = tokio::spawn(async move {
            let mut check = tokio::time::interval(CHECK_EVERY);
            check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    change = changes.recv() => {
                        let sent = match change {
                            Ok(frame) => outbound.send(frame.as_ref()),
                            // Dropped messages are not replayed; the browser is
                            // told it missed some and fetches the state afresh.
                            // Replaying would need a history, and the read API
                            // already is one.
                            Err(RecvError::Lagged(missed)) => {
                                outbound.send(LiveUpdate::Resync { missed }.to_frame().as_ref())
                            }
                            Err(RecvError::Closed) => break,
                        };
                        // The socket has gone; `on_close` is on its way.
                        if sent.is_err() {
                            break;
                        }
                    }
                    _ = check.tick() => {
                        let quiet = Utc::now().timestamp() - last_heard.load(Ordering::Relaxed);
                        if quiet > SILENCE.as_secs() as i64 {
                            let _ = outbound.close_with("no heartbeat");
                            break;
                        }
                    }
                }
            }
        });

        self.lock().insert(socket.id(), Connection { forwarder: forwarder.abort_handle(), heard });
        Ok(())
    }

    /// The only thing a browser sends is a heartbeat, and anything it sends
    /// counts as one. The reply is how the browser knows the server is still
    /// there — a browser cannot see WebSocket ping frames.
    async fn on_message(&self, socket: &Socket, message: Message) -> Result<()> {
        if let Some(connection) = self.lock().get(&socket.id()) {
            connection.heard.store(Utc::now().timestamp(), Ordering::Relaxed);
        }

        match message.as_text() {
            Some("ping") => socket.send(LiveUpdate::Pong.to_frame().as_ref()),
            // Anything else is ignored rather than answered with an error: this
            // endpoint takes no instructions, and there is nothing to explain.
            _ => Ok(()),
        }
    }

    async fn on_close(&self, socket: &Socket) {
        if let Some(connection) = self.lock().remove(&socket.id()) {
            connection.forwarder.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::websocket::Outbound;
    use tokio::sync::mpsc;

    fn socket() -> (Socket, mpsc::UnboundedReceiver<Outbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Socket::new(SocketId::next(), "/ws/live", Vec::new(), tx), rx)
    }

    /// The next frame the socket was asked to send, parsed.
    async fn next(rx: &mut mpsc::UnboundedReceiver<Outbound>) -> serde_json::Value {
        let outbound = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("a frame within two seconds")
            .expect("the socket is still open");
        match outbound {
            Outbound::Send(message) => {
                serde_json::from_str(message.as_text().expect("a text frame")).expect("JSON")
            }
            Outbound::Close(reason) => panic!("closed: {reason:?}"),
        }
    }

    #[tokio::test]
    async fn a_browser_is_greeted_and_then_hears_every_change() {
        let feed = LiveFeed::new();
        let handler = LiveSocket::new(feed.clone());
        let (socket, mut rx) = socket();

        handler.on_connect(&socket).await.unwrap();
        assert_eq!(next(&mut rx).await["type"], "hello");

        feed.publish(|| LiveUpdate::HostForgotten { mac: "18:66:da:11:22:33".into() });
        let heard = next(&mut rx).await;
        assert_eq!(heard["type"], "host.forgotten");
        assert_eq!(heard["data"]["mac"], "18:66:da:11:22:33");
    }

    #[tokio::test]
    async fn one_change_reaches_every_open_screen() {
        let feed = LiveFeed::new();
        let handler = LiveSocket::new(feed.clone());
        let (first, mut first_rx) = socket();
        let (second, mut second_rx) = socket();

        handler.on_connect(&first).await.unwrap();
        handler.on_connect(&second).await.unwrap();
        assert_eq!(handler.connections(), 2);
        next(&mut first_rx).await;
        next(&mut second_rx).await;

        feed.publish(|| LiveUpdate::Config { changed: vec!["SERVER_PORT".into()] });
        assert_eq!(next(&mut first_rx).await["type"], "config");
        assert_eq!(next(&mut second_rx).await["type"], "config");
    }

    #[tokio::test]
    async fn a_heartbeat_is_answered_and_anything_else_is_ignored() {
        let handler = LiveSocket::new(LiveFeed::new());
        let (socket, mut rx) = socket();
        handler.on_connect(&socket).await.unwrap();
        next(&mut rx).await;

        handler.on_message(&socket, Message::text("DELETE everything")).await.unwrap();
        handler.on_message(&socket, Message::text("ping")).await.unwrap();
        assert_eq!(next(&mut rx).await["type"], "pong", "the first thing back is the pong");
    }

    #[tokio::test]
    async fn a_closed_socket_stops_listening() {
        // The forwarder is stopped by `on_close`, not left to discover the
        // socket is gone on a send that, on a quiet network, never comes.
        let feed = LiveFeed::new();
        let handler = LiveSocket::new(feed.clone());
        let (socket, _rx) = socket();

        handler.on_connect(&socket).await.unwrap();
        assert_eq!(feed.watchers(), 1);

        handler.on_close(&socket).await;
        assert_eq!(handler.connections(), 0);

        // Aborting is asynchronous; the receiver is dropped once the task
        // is next polled.
        for _ in 0..50 {
            if feed.watchers() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(feed.watchers(), 0, "nothing is still subscribed for a closed socket");
    }

    #[tokio::test(start_paused = true)]
    async fn a_browser_that_goes_silent_is_disconnected() {
        let handler = LiveSocket::new(LiveFeed::new());
        let (socket, mut rx) = socket();
        handler.on_connect(&socket).await.unwrap();
        next(&mut rx).await;

        // The silence is measured on the wall clock, so it is aged directly
        // rather than waited for.
        if let Some(connection) = handler.lock().get(&socket.id()) {
            connection.heard.store(Utc::now().timestamp() - SILENCE.as_secs() as i64 - 1, Ordering::Relaxed);
        }
        tokio::time::advance(CHECK_EVERY * 2).await;

        let outbound = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(outbound, Outbound::Close(_)), "{outbound:?}");
    }
}
