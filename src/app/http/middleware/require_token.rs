//! The guard on everything that changes what a machine will boot.
//!
//! This server decides what code a fleet executes at power-on. An unguarded
//! endpoint that can pin a machine to a profile is therefore an unguarded
//! endpoint that can make a rack run an arbitrary image, so the mutating half
//! of the API sits behind a token — and when no token is configured it
//! **refuses** rather than running open.
//!
//! That refusal is the whole point. The alternative default, "no token means
//! no check", is the one that reads as convenient on a laptop and is a
//! remote-code-execution primitive on a boot VLAN, and nothing in the request
//! distinguishes the two.

use rainier_framework::http::{Request, Response, StatusCode};
use rainier_framework::middleware::{Middleware, Next};
use rainier_framework::prelude::*;

/// The header this accepts besides `Authorization: Bearer …`, for a `curl`
/// that does not want to think about bearer syntax.
pub const TOKEN_HEADER: &str = "x-pxe-token";

#[derive(Debug, Clone)]
pub struct RequireToken {
    token: Option<String>,
}

impl RequireToken {
    /// An empty or whitespace-only token is *no* token. A deployment that set
    /// `PXE_API_TOKEN=` meant to leave it unset, and treating the empty string
    /// as a valid secret would let an empty header through.
    pub fn new(token: impl Into<String>) -> Self {
        let token = token.into();
        Self { token: Some(token.trim().to_string()).filter(|token| !token.is_empty()) }
    }

    pub fn is_configured(&self) -> bool {
        self.token.is_some()
    }

    fn presented<'a>(&self, request: &'a Request) -> Option<&'a str> {
        request.bearer_token().or_else(|| request.header(TOKEN_HEADER))
    }
}

#[async_trait]
impl Middleware for RequireToken {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let Some(expected) = &self.token else {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "This endpoint changes what machines boot and no PXE_API_TOKEN is configured, so \
                 it is closed. Set PXE_API_TOKEN and restart.",
            );
        };

        match self.presented(&request) {
            Some(given) if constant_time_eq(given.as_bytes(), expected.as_bytes()) => {
                next.run(request).await
            }
            Some(_) => refuse(StatusCode::UNAUTHORIZED, "That token is not this server's token."),
            None => refuse(
                StatusCode::UNAUTHORIZED,
                "This endpoint needs a token: `Authorization: Bearer …` or an `x-pxe-token` \
                 header.",
            ),
        }
    }

    fn name(&self) -> &'static str {
        "RequireToken"
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    Response::json(&serde_json::json!({ "error": message })).with_status(status)
}

/// Compare without leaking where the difference is.
///
/// A byte-by-byte `==` returns as soon as two bytes differ, and the time that
/// takes is a measurement of how many leading bytes were right — which is
/// enough to recover a token one byte at a time over enough requests. The
/// length check leaks only the length, which is not a secret.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::middleware::Pipeline;

    async fn run(guard: RequireToken, request: Request) -> Response {
        Pipeline::new()
            .through(guard)
            .then(|_: Request| Box::pin(async { Response::text("through") }))
            .run(request)
            .await
    }

    fn with_header(name: &str, value: &str) -> Request {
        Request::builder().uri("/api/hosts/x/pin").header(name, value).build()
    }

    #[tokio::test]
    async fn no_configured_token_closes_the_endpoint_rather_than_opening_it() {
        // The default that matters. This is a boot server: "unguarded" means
        // anybody on the network chooses what the fleet executes.
        let response = run(RequireToken::new(""), Request::builder().uri("/x").build()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_whitespace_token_is_not_a_token() {
        let guard = RequireToken::new("   ");
        assert!(!guard.is_configured());
        assert_eq!(
            run(guard, with_header("x-pxe-token", "   ")).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_right_token_gets_through_either_way_it_is_presented() {
        for (header, value) in
            [("authorization", "Bearer s3cret"), ("x-pxe-token", "s3cret")]
        {
            let response = run(RequireToken::new("s3cret"), with_header(header, value)).await;
            assert_eq!(response.status(), StatusCode::OK, "{header}");
        }
    }

    #[tokio::test]
    async fn a_wrong_or_missing_token_is_refused() {
        assert_eq!(
            run(RequireToken::new("s3cret"), with_header("x-pxe-token", "nope")).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            run(RequireToken::new("s3cret"), Request::builder().uri("/x").build()).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn the_comparison_does_not_stop_at_the_first_wrong_byte() {
        // Timing-safe, because a token recovered one byte at a time is a token
        // recovered.
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}
