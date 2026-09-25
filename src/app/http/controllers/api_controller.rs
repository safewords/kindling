//! The management API.
//!
//! Reads are open; anything that changes what a machine will boot is behind
//! the token — see `middleware/require_token.rs` for why that asymmetry is
//! deliberate rather than lazy.

use rainier_framework::prelude::*;
use serde::Deserialize;

use crate::app::http::controllers::boot_controller::resolve;
use crate::app::http::requests::SetProfileRequest;
use crate::app::models::Host;
use crate::app::repositories::{BootEventRepository, HostRepository};
use crate::app::services::{BootService, RuleStore};
use crate::pxe::arch::{parse_arch, ClientArch};
use crate::pxe::facts::{ClientFacts, Stage};
use crate::pxe::mac::MacAddr;
use crate::pxe::policy;

/// `GET /api/health`
pub async fn health() -> Result<Response> {
    let rules = resolve::<RuleStore>()?;
    let current = rules.current();
    let hosts = resolve::<HostRepository>()?;

    // The inventory is reported on but not required: a boot server whose
    // database is down still boots machines, and a health check that said
    // otherwise would page somebody for the wrong thing.
    let (inventory, inventory_error) = match hosts.count().await {
        Ok(count) => (serde_json::json!({ "status": "ok", "hosts": count }), None),
        Err(e) => (serde_json::json!({ "status": "unreachable" }), Some(e.message().to_string())),
    };

    Ok(Response::json(&serde_json::json!({
        "status": "ok",
        "rules": {
            "source": current.source(),
            "revision": rules.revision(),
            "loaded_at": current.loaded_at(),
            "rules": current.rules().len(),
            "profiles": current.profiles().len(),
            // Present only when what is stored is not what is running.
            "last_reload_error": rules.last_error().map(|(at, message)| {
                serde_json::json!({ "at": at, "error": message })
            }),
        },
        "inventory": inventory,
        "inventory_error": inventory_error,
        "version": build_info!(),
    })))
}

/// `GET /api/hosts`
pub async fn hosts(request: Req) -> Result<Response> {
    let hosts = resolve::<HostRepository>()?;

    let page = request.input("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let per_page: u64 =
        request.input("per_page").and_then(|p| p.parse().ok()).unwrap_or(50).clamp(1, 500);

    let found = hosts
        .page(page, per_page, request.input("search").as_deref(), request.input("tag").as_deref())
        .await?;

    Ok(Response::json(&serde_json::json!({
        "data": found.data.iter().map(Host::as_json).collect::<Vec<_>>(),
        "total": found.total,
        "current_page": found.current_page,
        "per_page": found.per_page,
    })))
}

/// `GET /api/hosts/{host}`
pub async fn show_host(request: Req) -> Result<Response> {
    let mac = route_mac(&request)?;
    let host = find_host(mac).await?;
    let events = resolve::<BootEventRepository>()?.for_mac(mac, 50).await?;

    Ok(Response::json(&serde_json::json!({
        "host": host.as_json(),
        "events": events.iter().map(crate::app::models::BootEvent::as_json).collect::<Vec<_>>(),
    })))
}

/// `POST /api/hosts/{host}/pin`
pub async fn pin(request: Req, Validated(input): Validated<SetProfileRequest>) -> Result<Response> {
    let mac = route_mac(&request)?;
    known_profile(&input.profile)?;

    let hosts = resolve::<HostRepository>()?;
    if !hosts.pin(mac, Some(&input.profile)).await? {
        return Err(unknown_host(mac));
    }

    tracing::info!(%mac, profile = input.profile, "pinned by an operator");
    Ok(Response::json(&serde_json::json!({
        "mac": mac.to_string(),
        "pinned_profile": input.profile,
    })))
}

/// `DELETE /api/hosts/{host}/pin`
pub async fn unpin(request: Req) -> Result<Response> {
    let mac = route_mac(&request)?;
    let hosts = resolve::<HostRepository>()?;

    if !hosts.pin(mac, None).await? {
        return Err(unknown_host(mac));
    }
    Ok(Response::json(&serde_json::json!({ "mac": mac.to_string(), "pinned_profile": null })))
}

/// `POST /api/hosts/{host}/once`
pub async fn set_once(
    request: Req,
    Validated(input): Validated<SetProfileRequest>,
) -> Result<Response> {
    let mac = route_mac(&request)?;
    known_profile(&input.profile)?;

    let hosts = resolve::<HostRepository>()?;
    if !hosts.set_once(mac, Some(&input.profile)).await? {
        return Err(unknown_host(mac));
    }

    tracing::info!(%mac, profile = input.profile, "one-shot boot set by an operator");
    Ok(Response::json(&serde_json::json!({
        "mac": mac.to_string(),
        "once_profile": input.profile,
    })))
}

/// `DELETE /api/hosts/{host}/once`
pub async fn clear_once(request: Req) -> Result<Response> {
    let mac = route_mac(&request)?;
    let hosts = resolve::<HostRepository>()?;

    if !hosts.set_once(mac, None).await? {
        return Err(unknown_host(mac));
    }
    Ok(Response::json(&serde_json::json!({ "mac": mac.to_string(), "once_profile": null })))
}

#[derive(Debug, Default, Deserialize)]
pub struct TagChange {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// `POST /api/hosts/{host}/tags`
pub async fn tags(request: Req) -> Result<Response> {
    let mac = route_mac(&request)?;
    // Two lists rather than one field, so this is read as a document rather
    // than through the validator — whose rules describe scalars.
    let change: TagChange = request.json()?;

    let hosts = resolve::<HostRepository>()?;
    let mut host = find_host(mac).await?;

    let mut changed = false;
    for tag in &change.add {
        changed |= host.add_tag(tag.trim());
    }
    for tag in &change.remove {
        changed |= host.remove_tag(tag.trim());
    }

    if changed {
        hosts.save(&host).await?;
    }

    Ok(Response::json(&serde_json::json!({ "mac": mac.to_string(), "tags": host.tags.0 })))
}

/// `DELETE /api/hosts/{host}` — forget a machine entirely.
pub async fn forget(request: Req) -> Result<Response> {
    let mac = route_mac(&request)?;
    let hosts = resolve::<HostRepository>()?;

    if !hosts.forget(mac).await? {
        return Err(unknown_host(mac));
    }

    // Worth a line: the next time this machine boots it is new again, and a
    // rule on `known = false` will fire for it.
    tracing::info!(%mac, "forgotten; its next boot will look like a first sighting");
    Ok(Response::no_content())
}

/// `GET /api/events`
pub async fn events(request: Req) -> Result<Response> {
    let limit: u64 =
        request.input("limit").and_then(|l| l.parse().ok()).unwrap_or(100).clamp(1, 1000);
    let events = resolve::<BootEventRepository>()?.recent(limit).await?;

    Ok(Response::json(&serde_json::json!({
        "data": events.iter().map(crate::app::models::BootEvent::as_json).collect::<Vec<_>>(),
    })))
}

/// `GET /api/rules`
pub async fn rules() -> Result<Response> {
    let store = resolve::<RuleStore>()?;
    let rules = store.current();

    let listed: Vec<_> = rules
        .rules()
        .iter()
        .map(|rule| {
            serde_json::json!({
                "name": rule.name,
                "description": rule.description,
                "enabled": rule.enabled,
                "priority": rule.priority,
                "profile": rule.profile,
                "tag": rule.tag,
                "remove_tags": rule.remove_tags,
                "set": rule.set,
                "stops": rule.stops(),
                "when": rule.when,
                "unless": rule.unless,
            })
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "source": rules.source(),
        "revision": store.revision(),
        "loaded_at": rules.loaded_at(),
        "default_profile": rules.settings().default_profile,
        "bootloaders": rules.bootloaders(),
        "data": listed,
    })))
}

/// `GET /api/profiles`
pub async fn profiles() -> Result<Response> {
    let rules = resolve::<RuleStore>()?.current();

    let listed: Vec<_> = rules
        .profiles()
        .iter()
        .map(|(name, profile)| {
            serde_json::json!({
                "name": name,
                "label": profile.label_or(name),
                "description": profile.description,
                "kind": profile.kind(),
            })
        })
        .collect();

    Ok(Response::json(&serde_json::json!({ "data": listed })))
}

/// `POST /api/rules/reload`
pub async fn reload() -> Result<Response> {
    let store = resolve::<RuleStore>()?;

    match store.reload().await {
        Ok(rules) => Ok(Response::json(&serde_json::json!({
            "reloaded": true,
            "rules": rules.rules().len(),
            "profiles": rules.profiles().len(),
            "loaded_at": rules.loaded_at(),
        }))),
        // A 422 rather than a 500: the stored policy is wrong, the server is fine, and
        // the running policy is untouched. The body carries every problem at
        // once so a fix is one edit rather than five round trips.
        Err(e) => Ok(Response::json(&serde_json::json!({
            "reloaded": false,
            "problems": e.problems,
            "still_running": "the policy that was already loaded",
        }))
        .with_status(StatusCode::UNPROCESSABLE_ENTITY)),
    }
}

/// What `POST /api/rules/test` takes: a machine, described.
#[derive(Debug, Default, Deserialize)]
pub struct TestSubject {
    pub mac: Option<String>,
    pub arch: Option<String>,
    pub vendor_class: Option<String>,
    pub user_class: Option<String>,
    pub hostname: Option<String>,
    pub uuid: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub asset: Option<String>,
    pub client_ip: Option<String>,
    pub relay_ip: Option<String>,
    pub stage: Option<String>,
}

/// `POST /api/rules/test`
///
/// The endpoint the admin UI's "what would this machine boot?" box calls, and
/// the same code path `pxe:test` uses. Nothing is written: a machine described
/// here does not become a row, and a one-shot is not spent.
pub async fn test(request: Req) -> Result<Response> {
    let subject: TestSubject = request.json()?;
    let service = resolve::<BootService>()?;

    let mac: MacAddr = match subject.mac.as_deref() {
        Some(raw) => raw
            .parse()
            .map_err(|_| Error::bad_request(format!("`{raw}` is not a MAC address")))?,
        None => MacAddr::ZERO,
    };

    let arch = match subject.arch.as_deref() {
        Some(name) => parse_arch(name).ok_or_else(|| {
            Error::bad_request(format!(
                "`{name}` is not an architecture. Try one of: {}",
                ClientArch::known_labels().join(", ")
            ))
        })?,
        None => ClientArch::X64_UEFI,
    };

    let stage = match subject.stage.as_deref() {
        Some("firmware") | Some("dhcp") => Stage::Firmware,
        _ => Stage::Ipxe,
    };

    let facts = ClientFacts::new(mac, arch, stage)
        .with_vendor_class(subject.vendor_class.clone())
        .with_user_class(subject.user_class.clone())
        .with_hostname(subject.hostname.clone())
        .with_uuid(subject.uuid.clone())
        .with_smbios(
            subject.manufacturer.clone(),
            subject.product.clone(),
            subject.serial.clone(),
            subject.asset.clone(),
        )
        .with_client_ip(subject.client_ip.as_deref().and_then(|ip| ip.parse().ok()))
        .with_relay_ip(subject.relay_ip.as_deref().and_then(|ip| ip.parse().ok()))
        .identified(service.ouis());

    let (facts, decision) = service.dry_run(facts).await;
    let rules = service.rules().current();

    // The script too, because "which profile" and "what does that profile
    // actually send" are different questions and the second one is where the
    // mistakes are.
    let script = policy::ipxe_script(&decision, &facts, &rules, service.settings())
        .unwrap_or_else(|e| format!("# this profile could not be rendered: {e}"));

    let firmware = policy::firmware_answer(&decision, &facts, &rules, service.settings());

    Ok(Response::json(&serde_json::json!({
        "facts": facts,
        "decision": {
            "profile": decision.profile,
            "source": decision.source,
            "reason": decision.reason,
            "tags": decision.tags,
            "removed_tags": decision.evaluation.removed_tags,
            "vars": decision.evaluation.vars,
            "matched": decision.evaluation.matched,
            "used_default": decision.evaluation.used_default,
        },
        "trace": decision.evaluation.trace,
        "firmware": firmware.map(|answer| serde_json::json!({
            "file": answer.file,
            "over_http": answer.over_http,
            "reason": answer.reason,
        })),
        "script": script,
    })))
}

fn route_mac(request: &Request) -> Result<MacAddr> {
    let raw = request
        .route_param("host")
        .ok_or_else(|| Error::internal("the route has no `{host}` parameter"))?;

    raw.parse()
        .map_err(|_| Error::bad_request(format!("`{raw}` is not a MAC address")))
}

async fn find_host(mac: MacAddr) -> Result<Host> {
    resolve::<HostRepository>()?.by_mac(mac).await?.ok_or_else(|| unknown_host(mac))
}

fn unknown_host(mac: MacAddr) -> Error {
    Error::not_found(format!("no machine with the address {mac} has been seen"))
}

/// Refuse a pin at a profile that does not exist.
///
/// The alternative is discovering it at the machine: a pin is consulted before
/// the rules and a dangling one falls through, so the operator who set it
/// would watch the machine boot something else and have no idea why.
fn known_profile(profile: &str) -> Result<()> {
    let rules = resolve::<RuleStore>()?.current();
    if rules.profile(profile).is_some() {
        return Ok(());
    }

    let declared: Vec<&str> = rules.profiles().keys().map(String::as_str).collect();
    Err(Error::bad_request(format!(
        "`{profile}` is not a profile. Declared: {}",
        if declared.is_empty() { "none".to_string() } else { declared.join(", ") }
    )))
}

/// What a bulk action does to each machine named.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BulkAction {
    Pin,
    Unpin,
    Once,
    ClearOnce,
    Tag,
    Untag,
    Forget,
}

#[derive(Debug, Deserialize)]
pub struct BulkRequest {
    pub macs: Vec<String>,
    pub action: BulkAction,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `POST /api/hosts/bulk`
///
/// Selecting forty machines and pinning them one request at a time is forty
/// chances for the fortieth to fail silently, so the whole set is one call
/// that reports per machine.
///
/// Deliberately *not* transactional. These are independent facts about
/// independent machines: rolling back thirty-nine successes because the
/// fortieth machine has never been seen would be worse than saying so.
pub async fn bulk(request: Req) -> Result<Response> {
    let body: BulkRequest = request.json()?;

    if body.macs.is_empty() {
        return Err(Error::bad_request("no machines were named"));
    }
    // A bound, because this walks the list doing a write each time and an
    // unbounded list from an API client is a way to hold a connection open.
    if body.macs.len() > 500 {
        return Err(Error::bad_request(format!(
            "{} machines at once is more than this endpoint will take; 500 is the limit",
            body.macs.len()
        )));
    }

    // Checked once, before anything is written: a profile that does not exist
    // would leave half the fleet pinned to nothing.
    if matches!(body.action, BulkAction::Pin | BulkAction::Once) {
        match body.profile.as_deref() {
            Some(profile) => known_profile(profile)?,
            None => return Err(Error::bad_request("which profile?")),
        }
    }
    if matches!(body.action, BulkAction::Tag | BulkAction::Untag) && body.tags.is_empty() {
        return Err(Error::bad_request("which tags?"));
    }

    let hosts = resolve::<HostRepository>()?;
    let mut done: Vec<serde_json::Value> = Vec::with_capacity(body.macs.len());

    for raw in &body.macs {
        let Ok(mac) = raw.parse::<MacAddr>() else {
            done.push(serde_json::json!({ "mac": raw, "ok": false, "error": "not a MAC address" }));
            continue;
        };

        let outcome = match body.action {
            BulkAction::Pin => hosts.pin(mac, body.profile.as_deref()).await,
            BulkAction::Unpin => hosts.pin(mac, None).await,
            BulkAction::Once => hosts.set_once(mac, body.profile.as_deref()).await,
            BulkAction::ClearOnce => hosts.set_once(mac, None).await,
            BulkAction::Forget => hosts.forget(mac).await,
            BulkAction::Tag | BulkAction::Untag => match hosts.by_mac(mac).await {
                Ok(Some(mut host)) => {
                    let mut changed = false;
                    for tag in &body.tags {
                        changed |= match body.action {
                            BulkAction::Tag => host.add_tag(tag.trim()),
                            _ => host.remove_tag(tag.trim()),
                        };
                    }
                    match changed {
                        true => hosts.save(&host).await.map(|()| true),
                        // Nothing to write is still a success: the machine
                        // already carries the tags that were asked for.
                        false => Ok(true),
                    }
                }
                Ok(None) => Ok(false),
                Err(e) => Err(e),
            },
        };

        done.push(match outcome {
            Ok(true) => serde_json::json!({ "mac": mac.to_string(), "ok": true }),
            Ok(false) => serde_json::json!({
                "mac": mac.to_string(),
                "ok": false,
                "error": "this machine has not been seen here",
            }),
            Err(e) => serde_json::json!({
                "mac": mac.to_string(),
                "ok": false,
                "error": e.message(),
            }),
        });
    }

    let changed = done.iter().filter(|row| row["ok"] == true).count();
    tracing::info!(
        action = ?body.action,
        machines = body.macs.len(),
        changed,
        "bulk change by an operator"
    );

    Ok(Response::json(&serde_json::json!({
        "changed": changed,
        "total": body.macs.len(),
        "results": done,
    })))
}

/// `GET /api/tags` — every tag in use, with how many machines carry it.
///
/// What a filter menu is built from. Counted here rather than in the browser
/// because the browser only has the page it is looking at.
pub async fn tags_in_use() -> Result<Response> {
    let hosts = resolve::<HostRepository>()?.all().await?;

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut vendors: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut arches: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    for host in &hosts {
        for tag in &host.tags.0 {
            *counts.entry(tag.clone()).or_default() += 1;
        }
        if let Some(vendor) = &host.vendor {
            *vendors.entry(vendor.clone()).or_default() += 1;
        }
        *arches.entry(host.arch.clone()).or_default() += 1;
    }

    let rows = |map: std::collections::BTreeMap<String, usize>| {
        map.into_iter()
            .map(|(name, count)| serde_json::json!({ "name": name, "count": count }))
            .collect::<Vec<_>>()
    };

    Ok(Response::json(&serde_json::json!({
        "tags": rows(counts),
        "vendors": rows(vendors),
        "arches": rows(arches),
    })))
}
