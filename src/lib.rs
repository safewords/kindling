//! kindling — a network boot server with a rule engine.
//!
//! ```text
//! src/
//!   bootstrap.rs        assembles and boots the application
//!   config/             one module per concern, read from .env
//!   pxe/                the domain: DHCP, TFTP, rules, profiles, policy
//!     dhcp/             the proxy DHCP responder and its packets
//!     tftp/             a read-only TFTP server
//!     rules.rs          the policy file and its matcher
//!     profile.rs        what a machine is sent to boot
//!     policy.rs         facts + rules + overrides → a decision
//!   app/
//!     models/           the inventory and the boot log
//!     repositories/     data access
//!     services/         the rule store, and the piece that runs policy
//!     http/             controllers, middleware, the kernel
//!     console/commands/ pxe:serve, pxe:rules, pxe:test, …
//!   database/migrations/
//!   routes/
//! ```
//!
//! The split that matters is `pxe/` against `app/`. Everything in `pxe/` is a
//! pure function of its inputs — a packet in, a decision out — and is tested
//! without a database, a socket or a framework. `app/` is where that meets
//! storage, configuration and the network.

pub mod app;
pub mod bootstrap;
pub mod config;
pub mod database;
pub mod pxe;
pub mod routes;

pub use bootstrap::{boot, boot_with, environment, Mode};
