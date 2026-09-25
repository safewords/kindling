//! The body of a pin or a one-shot: one profile name.
//!
//! A contract rather than a `request.input("profile")` because of what the
//! contract's *other* half does — it hands the action only the fields the
//! rules named. The endpoint takes a profile name and nothing else, and this
//! is what makes that true of the code as well as of the documentation.

use rainier_framework::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SetProfileRequest {
    pub profile: String,
}

#[async_trait]
impl FormRequest for SetProfileRequest {
    fn rules() -> RuleSet {
        // The length bound is not decoration: the name goes into a script and
        // into a log line, and an unbounded string from an API client belongs
        // in neither.
        vec![field("profile", [Rule::Required, Rule::String, Rule::Between(1.0, 64.0)])]
    }

    // Authorisation is the token middleware's job, not this contract's. Saying
    // so here rather than leaving the default is deliberate — a reader should
    // not have to check whether a missing `authorize` means "anyone".
    async fn authorize(_request: &Request) -> bool {
        true
    }

    fn messages() -> Vec<(&'static str, &'static str)> {
        vec![("profile.required", "Name the profile this machine should boot.")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::http::Method;

    fn post(body: serde_json::Value) -> Request {
        Request::builder().method(Method::POST).json(&body).build()
    }

    #[tokio::test]
    async fn a_profile_name_becomes_a_typed_payload() {
        let validated =
            SetProfileRequest::validate_request(&post(serde_json::json!({ "profile": "ubuntu-2404" })))
                .await
                .expect("valid");
        assert_eq!(validated.profile, "ubuntu-2404");
    }

    #[tokio::test]
    async fn an_empty_body_is_refused_with_a_sentence_a_human_wrote() {
        // The summary is the framework's; the per-field text is ours, and it
        // is the part an API client shows somebody.
        let error =
            SetProfileRequest::validate_request(&post(serde_json::json!({}))).await.unwrap_err();

        let details = error.details().expect("a validation failure carries its fields").to_string();
        assert!(details.contains("Name the profile"), "{details}");
    }

    #[tokio::test]
    async fn a_name_nobody_could_have_meant_is_refused() {
        let long = "x".repeat(200);
        assert!(SetProfileRequest::validate_request(&post(serde_json::json!({ "profile": long })))
            .await
            .is_err());
    }
}
