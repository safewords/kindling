//! WebSocket handlers — routes that answered `101` instead of `200`.

pub mod live;

pub use live::LiveSocket;
