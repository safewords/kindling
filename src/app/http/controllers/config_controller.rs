//! Editing `.env` over HTTP.
//!
//! Configuration is read once, at boot, so every change here needs a restart
//! to take effect. The editor says so rather than implying otherwise — a
//! settings page that looks like it applied something and did not is worse
//! than one that is honest about the restart.
//!
//! What it does give is the thing a text editor cannot: a proposed file is run
//! through the *same* `configure` the server boots with before it is written.
//! You cannot save a `.env` that will not start.

use rainier_framework::config::Config;
use rainier_framework::prelude::*;
use serde::Deserialize;

use crate::app::http::controllers::boot_controller::resolve;
use crate::app::services::env_file::{EnvError, CATALOGUE};
use crate::app::services::{EnvFile, LiveFeed, LiveUpdate};
use crate::config::keys::*;

/// `GET /api/config` — the catalogue, the file, and what is running now.
pub async fn show() -> Result<Response> {
    let file = env_file();
    let settings = resolve::<Config>()?;

    // Grouped the way the catalogue orders them, so the editor renders
    // sections without deciding what belongs together.
    let mut sections: Vec<serde_json::Value> = Vec::new();
    for spec in CATALOGUE {
        if !sections.iter().any(|s| s["name"] == spec.section) {
            sections.push(serde_json::json!({ "name": spec.section }));
        }
    }

    Ok(Response::json(&serde_json::json!({
        "path": file.path().display().to_string(),
        "exists": file.path().exists(),
        "text": file.read(),
        "sections": sections,
        "settings": file.settings(),
        // What the process is *actually* running on, which is not the same as
        // what the file says once somebody has edited it without restarting.
        "running": {
            "server_ip": settings.get_or(PXE_SERVER_IP, String::new()),
            "http_base": settings.get_or(PXE_HTTP_BASE, String::new()),
            "tftp_root": settings.get_or(PXE_TFTP_ROOT, String::new()),
            "rules_path": settings.get_or(PXE_RULES_PATH, String::new()),
            "dhcp_enabled": settings.get_or(PXE_DHCP_ENABLED, false),
            "tftp_enabled": settings.get_or(PXE_TFTP_ENABLED, false),
            "tftp_port": settings.get_or(PXE_TFTP_PORT, 0u16),
            "loop_threshold": settings.get_or(PXE_LOOP_THRESHOLD, 0usize),
            "loop_window_secs": settings.get_or(PXE_LOOP_WINDOW_SECS, 0u64),
            "api_token_set": !settings.get_or(PXE_API_TOKEN, String::new()).trim().is_empty(),
        },
        "note": "Configuration is read at startup. Saving here writes the file; the server \
                 picks it up on its next start.",
    })))
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub text: String,
}

/// `POST /api/config/validate` — would this boot? Nothing is written.
pub async fn validate(request: Req) -> Result<Response> {
    let document: Document = request.json()?;

    Ok(match EnvFile::validate(&document.text) {
        Ok(()) => Response::json(&serde_json::json!({ "ok": true })),
        Err(e) => Response::json(&serde_json::json!({ "ok": false, "problems": e.problems })),
    })
}

/// `PUT /api/config` — replace the whole file.
pub async fn replace(request: Req) -> Result<Response> {
    let document: Document = request.json()?;
    let file = env_file();

    file.write(&document.text).map_err(unprocessable)?;
    tracing::info!(path = %file.path().display(), "configuration replaced");

    Ok(saved(&["the whole file".to_string()]))
}

#[derive(Debug, Deserialize)]
pub struct Changes {
    /// `KEY` to a value, or to `null` to comment the setting out.
    pub changes: std::collections::BTreeMap<String, Option<String>>,
}

/// `PATCH /api/config` — change some values, leaving the rest of the file as
/// it was written.
pub async fn patch(request: Req) -> Result<Response> {
    let body: Changes = request.json()?;

    if body.changes.is_empty() {
        return Err(Error::bad_request("no changes were sent"));
    }

    // A key nobody has heard of is almost always a typo, and a typo written
    // into `.env` is a setting that silently does nothing for ever.
    let unknown: Vec<&String> = body
        .changes
        .keys()
        .filter(|key| crate::app::services::env_file::spec_for(key).is_none())
        .collect();

    if !unknown.is_empty() {
        return Err(Error::bad_request(format!(
            "this server reads no setting called {}. A name it does not know would sit in the \
             file doing nothing.",
            unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        )));
    }

    let changes: Vec<(String, Option<String>)> =
        body.changes.iter().map(|(key, value)| (key.clone(), value.clone())).collect();

    let file = env_file();
    file.apply(&changes).map_err(unprocessable)?;

    let changed: Vec<String> = body.changes.keys().cloned().collect();
    tracing::info!(settings = changed.join(", "), "configuration edited");

    Ok(saved(&changed))
}

/// The answer to a write, and the announcement of it.
///
/// Both writing endpoints end here, which makes it the one place `.env`
/// changes can be announced from. The announcement carries the setting
/// *names* and never their values: watching needs no token, and one of these
/// settings is the token.
fn saved(changed: &[String]) -> Response {
    if let Ok(live) = resolve::<LiveFeed>() {
        live.publish(|| LiveUpdate::Config { changed: changed.to_vec() });
    }

    Response::json(&serde_json::json!({
        "saved": true,
        "changed": changed,
        "restart_required": true,
        "note": "Written. The server reads configuration at startup, so this takes effect on its \
                 next start. The policy file is the exception — that reloads on demand.",
    }))
}

/// The `.env` beside the running server.
///
/// Fixed rather than configurable: the path configuration is read *from*
/// cannot itself be a setting without a chicken-and-egg problem, and the
/// framework looks here.
fn env_file() -> EnvFile {
    EnvFile::at(".env")
}

fn unprocessable(e: EnvError) -> Error {
    Error::bad_request(e.problems.join("\n"))
        .with_details(serde_json::json!({ "problems": e.problems }))
}
