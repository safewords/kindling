//! Middleware, attached by value.
//!
//! Note what is *not* here. There is no session, no CSRF and no cookie on any
//! of it: the clients are firmware, iPXE and `curl`, none of which carry a
//! cookie, and a boot server with a login form would be a boot server nobody
//! can script against. Authorisation is one token on the endpoints that change
//! what machines boot.

use std::sync::Arc;

use rainier_framework::config::Config;
use rainier_framework::middleware::{AddHeaders, MiddlewareRegistry, MiddlewareStack};
use rainier_framework::telemetry::Trace;

use crate::app::http::middleware::RequireToken;
use crate::config::keys::PXE_API_TOKEN;

pub fn register(registry: &MiddlewareRegistry, trace: Option<Trace>) {
    if let Some(trace) = trace {
        registry.global(trace);
    }
    registry.global(AddHeaders::security_defaults());
}

/// The boot endpoints: no throttle at all, deliberately.
///
/// A rack powering on after a site outage is exactly the traffic a rate
/// limiter is built to stop, and stopping it means machines that do not come
/// back. The expensive work behind these routes is one indexed query and a
/// string; the protection that matters is that they change nothing.
pub fn boot() -> MiddlewareStack {
    MiddlewareStack::new()
}

/// Read-only API and the admin pages.
///
/// No rate limiter, and that is a decision rather than an omission. The
/// framework warns — correctly — that a throttle counting in process memory
/// states a limit that is multiplied by however many replicas are running, so
/// it is a number nobody can act on. What actually guards this surface is the
/// token below, and what actually makes the reads cheap is that they are
/// indexed queries. A deployment that needs a real limit wants one in the
/// proxy in front, where the count is of the whole service.
pub fn api() -> MiddlewareStack {
    MiddlewareStack::new()
}

/// Anything that changes what a machine will boot.
pub fn guarded() -> MiddlewareStack {
    api().with_stack(
        MiddlewareStack::new().resolved(|settings: Arc<Config>| {
            RequireToken::new(settings.get_or(PXE_API_TOKEN, String::new()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_path_is_not_rate_limited() {
        // A rack coming back after an outage is not an attack, and treating it
        // as one is how half a rack stays down.
        assert!(boot().labels().is_empty(), "{:?}", boot().labels());
    }

    #[test]
    fn writing_endpoints_carry_the_token_guard_and_reading_ones_do_not() {
        assert!(guarded().labels().contains(&"RequireToken"));
        assert!(!api().labels().contains(&"RequireToken"));
    }

    #[test]
    fn the_guarded_stack_is_the_api_stack_plus_the_guard() {
        // Composition rather than a second list to keep in step.
        assert_eq!(guarded().labels(), vec!["RequireToken"]);
    }

    #[test]
    fn security_headers_are_global() {
        let registry = MiddlewareRegistry::new();
        register(&registry, None);
        assert!(registry.global_labels().contains(&"AddHeaders"));
    }
}
