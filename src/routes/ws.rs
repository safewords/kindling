//! What a browser can hold open.
//!
//! One socket, and it only listens. Declared apart from `web.rs` and `api.rs`
//! because it is not a route the router sees: an upgrade is taken off the
//! accept loop before routing, so none of the middleware in `kernel.rs`
//! applies to it. That is fine for a feed of what the read API already shows,
//! and it is the thing to remember before adding anything here that writes.

use rainier_framework::prelude::*;

use crate::app::http::sockets::LiveSocket;
use crate::app::services::LiveFeed;

pub fn routes(feed: LiveFeed) -> WebSocketRoutes {
    WebSocketRoutes::new().add("/ws/live", LiveSocket::new(feed))
}
