//! The boot policy over HTTP — what the web UI's policy screen, rule wizard
//! and profile wizard are built on.
//!
//! Every write goes through [`RuleStore::apply`], so three things are true of
//! all of them without each endpoint restating it: nothing that does not
//! validate is stored, every change is kept as a revision, and the running
//! policy is swapped the moment the change is — there is no "saved but not
//! loaded" state for anybody to be confused by.
//!
//! A write may say which revision it was made against, in the
//! `x-policy-revision` header. When it does and the policy has moved on, it is
//! refused with a 409 rather than applied over a change its author never saw.

use std::collections::BTreeMap;

use rainier_framework::prelude::*;
use serde::Deserialize;

use crate::app::http::controllers::boot_controller::resolve;
use crate::app::repositories::{HostRepository, PolicyRepository};
use crate::app::services::{BootService, EditError, EditErrorKind, EditMeta, RuleStore};
use crate::pxe::arch::{parse_arch, ClientArch};
use crate::pxe::condition;
use crate::pxe::facts::{ClientFacts, Stage};
use crate::pxe::mac::MacAddr;
use crate::pxe::policy::{self, Overrides};
use crate::pxe::profile::{self, Profile, RenderContext};
use crate::pxe::rules::{Origin, PolicyDocument, Rule, RuleSet};

const REVISION_HEADER: &str = "x-policy-revision";
const ACTOR_HEADER: &str = "x-pxe-actor";

// ---------------------------------------------------------------------------
// Reading.
// ---------------------------------------------------------------------------

/// `GET /api/policy` — the running policy, rule by rule, with each rule's
/// conditions also in words.
pub async fn show() -> Result<Response> {
    let store = resolve::<RuleStore>()?;
    let rules = store.current();
    let latest = match store.repository() {
        Some(repository) => repository.latest().await.ok().flatten(),
        None => None,
    };

    Ok(Response::json(&serde_json::json!({
        "source": rules.source(),
        "revision": store.revision(),
        "last_change": latest.map(|r| r.as_summary()),
        "loaded_at": rules.loaded_at(),
        "last_error": store.last_error().map(|(at, message)| {
            serde_json::json!({ "at": at, "error": message })
        }),
        "settings": rules.settings(),
        "bootloaders": rules.bootloaders(),
        "rules": rules.rules().iter().map(|rule| rule_json(&rules, rule)).collect::<Vec<_>>(),
        "profiles": rules.profiles().iter().map(|(name, profile)| profile_json(&rules, name, profile)).collect::<Vec<_>>(),
        "document": rules.to_document(),
    })))
}

fn rule_json(rules: &RuleSet, rule: &Rule) -> serde_json::Value {
    let (when, unless) = rules.describe(&rule.name).unwrap_or_default();
    let mut json = serde_json::to_value(rule).unwrap_or_default();
    json["stops"] = serde_json::json!(rule.stops());
    json["when"] = serde_json::to_value(&rule.when).unwrap_or_default();
    json["unless"] = serde_json::to_value(&rule.unless).unwrap_or_default();
    json["tag"] = serde_json::json!(rule.tag);
    json["remove_tags"] = serde_json::json!(rule.remove_tags);
    json["set"] = serde_json::json!(rule.set);
    json["described"] = serde_json::json!({ "when": when, "unless": unless });
    json
}

fn profile_json(rules: &RuleSet, name: &str, profile: &Profile) -> serde_json::Value {
    // Who points at this profile: what deleting or renaming it would touch.
    let used_by: Vec<&str> = rules
        .rules()
        .iter()
        .filter(|rule| rule.profile.as_deref() == Some(name))
        .map(|rule| rule.name.as_str())
        .collect();
    let in_menus: Vec<&str> = rules
        .profiles()
        .iter()
        .filter(|(_, other)| {
            other.entries.iter().any(|e| e.profile == name) || other.default.as_deref() == Some(name)
        })
        .map(|(menu, _)| menu.as_str())
        .collect();

    serde_json::json!({
        "name": name,
        "label": profile.label_or(name),
        "kind": profile.kind(),
        "body": profile,
        "used_by": used_by,
        "in_menus": in_menus,
        "is_default": rules.settings().default_profile.as_deref() == Some(name),
    })
}

/// `GET /api/policy/schema` — everything an editor needs to know to draw a
/// form: the facts a condition can ask about and the operators each takes,
/// the kinds of profile, the placeholders a template can use, the
/// architectures a boot loader can be named for.
pub async fn schema() -> Result<Response> {
    let mut schema = condition::schema();
    schema["profile_kinds"] = serde_json::json!([
        { "kind": "kernel", "label": "Kernel and initrd", "help": "The common case: a kernel, its initrds and a command line. This server writes the iPXE." },
        { "kind": "script", "label": "iPXE script", "help": "Verbatim iPXE, for when the generated version is not enough." },
        { "kind": "menu", "label": "Menu", "help": "A list of other profiles for somebody standing at the machine, with a timeout and a default." },
        { "kind": "local", "label": "Boot the local disk", "help": "Stop network booting and hand control back to the disk. What most machines should do most of the time." },
        { "kind": "ignore", "label": "Do not answer", "help": "This server stays silent: for hardware with its own boot server, or that must never be touched." },
    ]);
    schema["placeholders"] = serde_json::json!(profile::PLACEHOLDERS);
    schema["architectures"] = serde_json::json!(ClientArch::known_labels());
    Ok(Response::json(&schema))
}

/// What the validate and preview endpoints take: a whole document, a TOML or
/// JSON text, or one rule to try in place of another.
#[derive(Debug, Default, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub document: Option<PolicyDocument>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub rule: Option<Rule>,
    /// The rule `rule` replaces, when editing rather than adding.
    #[serde(default)]
    pub original: Option<String>,
    /// Remove this rule instead — "what would deleting it do".
    #[serde(default)]
    pub remove: Option<String>,
}

impl Candidate {
    /// The policy as it would be with this change, validated.
    fn resolve(self, current: &RuleSet) -> std::result::Result<(RuleSet, Option<String>), EditError> {
        let mut focus = None;
        let document = if let Some(document) = self.document {
            document
        } else if let Some(text) = self.text {
            PolicyDocument::from_text(&text)?
        } else {
            let mut document = current.to_document();
            if let Some(rule) = self.rule {
                focus = Some(rule.name.clone());
                put_rule(&mut document, self.original.as_deref(), rule)?;
            }
            if let Some(name) = self.remove {
                take_rule(&mut document, &name)?;
            }
            document
        };
        Ok((RuleSet::from_document(document, Origin::Inline)?, focus))
    }
}

/// `POST /api/policy/validate` — would this load? Nothing is stored.
///
/// Open rather than behind the token: checking changes nothing, and an editor
/// that can only validate by saving encourages saving to find out.
pub async fn validate(request: Req) -> Result<Response> {
    let candidate: Candidate = request.json()?;
    let current = resolve::<RuleStore>()?.current();

    Ok(match candidate.resolve(&current) {
        Ok((rules, focus)) => {
            let focused = focus.as_deref().and_then(|name| {
                let position = rules.rules().iter().position(|r| r.name == name)?;
                let rule = &rules.rules()[position];
                Some(serde_json::json!({
                    "position": position,
                    "of": rules.rules().len(),
                    "rule": rule_json(&rules, rule),
                    // The neighbours in evaluation order, so the wizard can
                    // say "runs after X, before Y".
                    "after": position.checked_sub(1).map(|i| rules.rules()[i].name.clone()),
                    "before": rules.rules().get(position + 1).map(|r| r.name.clone()),
                }))
            });
            Response::json(&serde_json::json!({
                "ok": true,
                "rules": rules.rules().len(),
                "profiles": rules.profiles().len(),
                "focus": focused,
            }))
        }
        Err(e) => Response::json(&serde_json::json!({ "ok": false, "problems": e.problems })),
    })
}

/// `POST /api/policy/preview` — what a change would do to the machines in the
/// inventory, before it is made.
///
/// Every known machine is decided twice, against the running policy and the
/// candidate, and the ones whose answer changes — or that the rule being
/// edited fires for — are listed. This is the question a wizard should answer
/// before its last button: not "is it valid" but "which of my machines does
/// this reimage".
pub async fn preview(request: Req) -> Result<Response> {
    let candidate: Candidate = request.json()?;
    let store = resolve::<RuleStore>()?;
    let current = store.current();
    let (proposed, focus) = match candidate.resolve(&current) {
        Ok(resolved) => resolved,
        Err(e) => {
            return Ok(Response::json(&serde_json::json!({ "ok": false, "problems": e.problems })))
        }
    };

    let service = resolve::<BootService>()?;
    let hosts = resolve::<HostRepository>()?.all().await?;

    let mut rows = Vec::new();
    let (mut fires, mut changes) = (0usize, 0usize);
    for host in &hosts {
        let Some(facts) = host.facts(service.ouis()) else { continue };
        let overrides = host.overrides();
        let before = policy::decide(&current, &facts, &overrides);
        let after = policy::decide(&proposed, &facts, &overrides);

        let fired = focus.as_deref().is_some_and(|name| after.evaluation.matched.iter().any(|m| m == name));
        let changed = before.profile != after.profile;
        fires += usize::from(fired);
        changes += usize::from(changed);

        if (fired || changed) && rows.len() < 500 {
            rows.push(serde_json::json!({
                "mac": host.mac,
                "hostname": host.hostname,
                "vendor": host.vendor,
                "product": host.product,
                "tags": host.tags.0,
                "before": before.profile,
                "after": after.profile,
                "source": after.source,
                "reason": after.reason,
                "fires": fired,
                "changed": changed,
                // A pin or a one-shot outranks every rule, so a rule firing
                // for this machine will not change what it boots — worth
                // saying rather than leaving the operator puzzled.
                "overridden": overrides.pinned_profile.is_some() || overrides.once_profile.is_some(),
                "adds_tags": after.evaluation.tags,
                "removes_tags": after.evaluation.removed_tags,
                "vars": after.evaluation.vars,
            }));
        }
    }

    Ok(Response::json(&serde_json::json!({
        "ok": true,
        "hosts": hosts.len(),
        "fires": fires,
        "changes": changes,
        "rows": rows,
    })))
}

#[derive(Debug, Deserialize)]
pub struct RenderRequest {
    /// The profile to render, as it is being edited.
    pub profile: Profile,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

/// `POST /api/policy/profiles/render` — the iPXE a profile would produce, for
/// a machine, before it is saved.
pub async fn render_profile(request: Req) -> Result<Response> {
    let body: RenderRequest = request.json()?;
    let service = resolve::<BootService>()?;
    let rules = service.rules().current();
    let name = body.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "preview".into());

    let problems = body.profile.problems(&name);
    let mac: MacAddr = body
        .mac
        .as_deref()
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| "18:66:da:11:22:33".parse().unwrap_or(MacAddr::ZERO));
    let arch = body.arch.as_deref().and_then(parse_arch).unwrap_or(ClientArch::X64_UEFI);

    // The machine as the inventory knows it, if it does.
    let host = resolve::<HostRepository>()?.by_mac(mac).await.ok().flatten();
    let facts = host
        .as_ref()
        .and_then(|h| h.facts(service.ouis()))
        .unwrap_or_else(|| ClientFacts::new(mac, arch, Stage::Ipxe).identified(service.ouis()));
    let overrides = host.as_ref().map(|h| h.overrides()).unwrap_or_else(Overrides::default);
    let vars = policy::decide(&rules, &facts, &overrides).evaluation.vars;

    // Render against the running profiles plus this one, so a menu being
    // edited can list profiles that exist.
    let mut profiles = rules.profiles().clone();
    profiles.insert(name.clone(), body.profile.clone());

    let settings = service.settings();
    let context = RenderContext {
        server: settings.server_ip.to_string(),
        base: settings.http_base.trim_end_matches('/').to_string(),
        facts: &facts,
        profiles: &profiles,
        vars: &vars,
    };

    Ok(Response::json(&match profile::render(&name, &body.profile, &context) {
        Ok(script) => serde_json::json!({ "ok": problems.is_empty(), "script": script, "problems": problems, "mac": mac.to_string() }),
        Err(e) => {
            let problems: Vec<String> = problems.into_iter().chain([e.to_string()]).collect();
            serde_json::json!({ "ok": false, "script": null, "problems": problems, "mac": mac.to_string() })
        }
    }))
}

/// `GET /api/policy/export?format=json|toml`
pub async fn export(request: Req) -> Result<Response> {
    let document = resolve::<RuleStore>()?.current().to_document();
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

    Ok(match request.input("format").as_deref() {
        Some("toml") => {
            let text = document.to_toml().map_err(Error::internal)?;
            Response::download(text.into_bytes(), &format!("pxe-policy-{stamp}.toml"))
        }
        _ => {
            let text = serde_json::to_string_pretty(&document).map_err(|e| Error::internal(e.to_string()))?;
            Response::download(text.into_bytes(), &format!("pxe-policy-{stamp}.json"))
        }
    })
}

/// `GET /api/policy/revisions`
pub async fn revisions(request: Req) -> Result<Response> {
    let limit: u64 = request.input("limit").and_then(|l| l.parse().ok()).unwrap_or(100).clamp(1, 500);
    let store = resolve::<RuleStore>()?;
    let list = match store.repository() {
        Some(repository) => repository.revisions(limit).await?,
        None => Vec::new(),
    };
    Ok(Response::json(&serde_json::json!({
        "current": store.revision(),
        "data": list.iter().map(|r| r.as_summary()).collect::<Vec<_>>(),
    })))
}

/// `GET /api/policy/revisions/{revision}` — one version, whole.
pub async fn show_revision(request: Req) -> Result<Response> {
    let id = revision_param(&request)?;
    let repository = resolve::<PolicyRepository>()?;
    let revision = repository
        .revision(id)
        .await?
        .ok_or_else(|| Error::not_found(format!("there is no revision {id}")))?;

    let mut json = revision.as_summary();
    json["document"] = serde_json::to_value(&revision.document.0).unwrap_or_default();
    Ok(Response::json(&json))
}

// ---------------------------------------------------------------------------
// Writing.
// ---------------------------------------------------------------------------

/// `PUT /api/policy` — replace the whole policy: from a document, or from a
/// TOML or JSON text (an import).
pub async fn replace(request: Req) -> Result<Response> {
    let body: Candidate = request.json()?;

    let (document, summary) = match (body.document, body.text) {
        (Some(document), _) => (document, "the whole policy was replaced"),
        (None, Some(text)) => (
            PolicyDocument::from_text(&text).map_err(|e| edit_error(e.into()))?,
            "a policy was imported",
        ),
        (None, None) => return Err(Error::bad_request("send a `document`, or a `text` to import")),
    };
    let meta = meta(&request, summary);

    let store = resolve::<RuleStore>()?;
    changed(store.replace(meta, document).await)
}

/// `POST /api/policy/rules` — add a rule.
pub async fn add_rule(request: Req) -> Result<Response> {
    let rule: Rule = request.json()?;
    let meta = meta(&request, &format!("rule `{}` added", rule.name));
    let store = resolve::<RuleStore>()?;
    changed(store.apply(meta, move |document| put_rule(document, None, rule)).await)
}

/// `PUT /api/policy/rules/{rule}` — replace a rule, and rename it if the body
/// says a different name.
pub async fn put_rule_endpoint(request: Req) -> Result<Response> {
    let name = route_param(&request, "rule")?;
    let rule: Rule = request.json()?;
    let summary = if rule.name == name {
        format!("rule `{name}` updated")
    } else {
        format!("rule `{name}` updated and renamed to `{}`", rule.name)
    };
    let meta = meta(&request, &summary);
    let store = resolve::<RuleStore>()?;
    changed(store.apply(meta, move |document| put_rule(document, Some(&name), rule)).await)
}

/// `PATCH /api/policy/rules/{rule}` — change some of a rule's fields.
///
/// A JSON merge over the rule as it stands: a field sent replaces that field,
/// a field sent as `null` is cleared, a field left out is left alone. `when`
/// is replaced whole — removing a condition is the common edit, and a merge
/// could not express it.
pub async fn patch_rule(request: Req) -> Result<Response> {
    let name = route_param(&request, "rule")?;
    let patch: serde_json::Value = request.json()?;
    let serde_json::Value::Object(patch) = patch else {
        return Err(Error::bad_request("a patch is an object of the fields to change"));
    };
    let meta = meta(&request, &format!("rule `{name}` updated"));
    let store = resolve::<RuleStore>()?;

    changed(
        store
            .apply(meta, move |document| {
                let existing = document
                    .rule(&name)
                    .ok_or_else(|| EditError::not_found(format!("no rule is called `{name}`")))?;
                let mut merged = serde_json::to_value(existing)
                    .map_err(|e| EditError::invalid(e.to_string()))?;
                for (key, value) in patch {
                    if value.is_null() {
                        merged.as_object_mut().map(|o| o.remove(&key));
                    } else {
                        merged[key] = value;
                    }
                }
                let rule: Rule = serde_json::from_value(merged)
                    .map_err(|e| EditError::invalid(format!("the rule will not parse: {e}")))?;
                put_rule(document, Some(&name), rule)
            })
            .await,
    )
}

/// `DELETE /api/policy/rules/{rule}`
pub async fn remove_rule(request: Req) -> Result<Response> {
    let name = route_param(&request, "rule")?;
    let meta = meta(&request, &format!("rule `{name}` removed"));
    let store = resolve::<RuleStore>()?;
    changed(store.apply(meta, move |document| take_rule(document, &name).map(|_| ())).await)
}

#[derive(Debug, Deserialize)]
pub struct Enabled {
    pub enabled: bool,
}

/// `POST /api/policy/rules/{rule}/enabled` — the one-click edit, the one
/// somebody makes at 3am.
pub async fn set_rule_enabled(request: Req) -> Result<Response> {
    let name = route_param(&request, "rule")?;
    let body: Enabled = request.json()?;
    let meta = meta(
        &request,
        &format!("rule `{name}` {}", if body.enabled { "enabled" } else { "disabled" }),
    );
    let store = resolve::<RuleStore>()?;
    changed(
        store
            .apply(meta, move |document| {
                document
                    .rule_mut(&name)
                    .ok_or_else(|| EditError::not_found(format!("no rule is called `{name}`")))?
                    .enabled = body.enabled;
                Ok(())
            })
            .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct Reorder {
    /// Every rule's name, in the order they should run.
    pub order: Vec<String>,
}

/// `POST /api/policy/rules/reorder` — put the rules in this order.
///
/// Order is priority, so this rewrites priorities: tens, descending, which
/// leaves room to slot a rule between two others by hand later.
pub async fn reorder_rules(request: Req) -> Result<Response> {
    let body: Reorder = request.json()?;
    let meta = meta(&request, "rules reordered");
    let store = resolve::<RuleStore>()?;
    changed(
        store
            .apply(meta, move |document| {
                let mut named: BTreeMap<String, Rule> =
                    document.rules.drain(..).map(|r| (r.name.clone(), r)).collect();
                let count = body.order.len() as i64;
                let mut ordered = Vec::with_capacity(named.len());
                for (index, name) in body.order.iter().enumerate() {
                    let mut rule = named.remove(name).ok_or_else(|| {
                        EditError::not_found(format!("no rule is called `{name}`"))
                    })?;
                    rule.priority = (count - index as i64) * 10;
                    ordered.push(rule);
                }
                if !named.is_empty() {
                    return Err(EditError::invalid(format!(
                        "the new order leaves out {}; name every rule",
                        named.keys().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                    )));
                }
                document.rules = ordered;
                Ok(())
            })
            .await,
    )
}

/// `PUT /api/policy/profiles/{profile}` — save a profile. The body is the
/// profile's fields, plus an optional `rename_to`: a rename carries every rule,
/// menu entry and default that pointed at the old name with it.
pub async fn put_profile(request: Req) -> Result<Response> {
    let name = route_param(&request, "profile")?;
    let mut body: serde_json::Value = request.json()?;
    let rename_to = body
        .as_object_mut()
        .and_then(|o| o.remove("rename_to"))
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|new| !new.is_empty() && *new != name);
    let profile: Profile = serde_json::from_value(body)
        .map_err(|e| Error::bad_request(format!("the profile will not parse: {e}")))?;

    let summary = match &rename_to {
        Some(new) => format!("profile `{name}` saved and renamed to `{new}`"),
        None => format!("profile `{name}` saved"),
    };
    let meta = meta(&request, &summary);
    let store = resolve::<RuleStore>()?;

    changed(
        store
            .apply(meta, move |document| {
                let Some(new) = rename_to else {
                    document.profiles.insert(name, profile);
                    return Ok(());
                };
                if document.profiles.contains_key(&new) {
                    return Err(EditError::invalid(format!("a profile called `{new}` already exists")));
                }
                document.profiles.remove(&name);
                document.profiles.insert(new.clone(), profile);
                rename_profile_references(document, &name, &new);
                Ok(())
            })
            .await,
    )
}

fn rename_profile_references(document: &mut PolicyDocument, from: &str, to: &str) {
    let rename = |slot: &mut Option<String>| {
        if slot.as_deref() == Some(from) {
            *slot = Some(to.to_string());
        }
    };
    for rule in &mut document.rules {
        rename(&mut rule.profile);
    }
    for profile in document.profiles.values_mut() {
        rename(&mut profile.default);
        for entry in &mut profile.entries {
            if entry.profile == from {
                entry.profile = to.to_string();
            }
        }
    }
    rename(&mut document.settings.default_profile);
}

/// `DELETE /api/policy/profiles/{profile}`
///
/// Refused while anything still points at it — the validator will not store
/// a rule that boots nothing — and the refusal names what does.
pub async fn remove_profile(request: Req) -> Result<Response> {
    let name = route_param(&request, "profile")?;
    let meta = meta(&request, &format!("profile `{name}` removed"));
    let store = resolve::<RuleStore>()?;
    changed(
        store
            .apply(meta, move |document| {
                document
                    .profiles
                    .remove(&name)
                    .map(|_| ())
                    .ok_or_else(|| EditError::not_found(format!("no profile is called `{name}`")))
            })
            .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct PolicySettings {
    /// Present and `null` clears it; absent leaves it.
    #[serde(default, deserialize_with = "double_option")]
    pub default_profile: Option<Option<String>>,
    #[serde(default)]
    pub timezone_offset_minutes: Option<i64>,
}

fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// `PUT /api/policy/settings`
pub async fn put_settings(request: Req) -> Result<Response> {
    let body: PolicySettings = request.json()?;
    let meta = meta(&request, "policy settings changed");
    let store = resolve::<RuleStore>()?;
    changed(
        store
            .apply(meta, move |document| {
                if let Some(default) = body.default_profile {
                    document.settings.default_profile = default.filter(|p| !p.trim().is_empty());
                }
                if let Some(offset) = body.timezone_offset_minutes {
                    document.settings.timezone_offset_minutes = offset;
                }
                Ok(())
            })
            .await,
    )
}

#[derive(Debug, Deserialize)]
pub struct Bootloader {
    /// An explicit `null` removes the entry.
    pub file: Option<String>,
}

/// `PUT /api/policy/bootloaders/{arch}`
pub async fn put_bootloader(request: Req) -> Result<Response> {
    let arch = route_param(&request, "arch")?;
    let body: Bootloader = request.json()?;
    let meta = meta(&request, &format!("the boot loader for `{arch}` was changed"));
    let store = resolve::<RuleStore>()?;
    changed(
        store
            .apply(meta, move |document| {
                match body.file.map(|f| f.trim().to_string()).filter(|f| !f.is_empty()) {
                    Some(file) => {
                        document.bootloaders.insert(arch, file);
                    }
                    None => {
                        document.bootloaders.remove(&arch);
                    }
                }
                Ok(())
            })
            .await,
    )
}

/// `POST /api/policy/revisions/{revision}/restore` — roll back.
pub async fn restore_revision(request: Req) -> Result<Response> {
    let id = revision_param(&request)?;
    let store = resolve::<RuleStore>()?;
    changed(store.restore(id, &actor(&request)).await)
}

// ---------------------------------------------------------------------------
// The wizard's saved templates.
// ---------------------------------------------------------------------------

/// `GET /api/policy/templates`
pub async fn templates() -> Result<Response> {
    let list = resolve::<PolicyRepository>()?.templates().await?;
    Ok(Response::json(&serde_json::json!({
        "data": list.iter().map(|t| t.as_json()).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct TemplateBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// Any of a rule's fields.
    pub rule: serde_json::Value,
}

/// `POST /api/policy/templates` — save one, replacing any of the same name.
pub async fn save_template(request: Req) -> Result<Response> {
    let body: TemplateBody = request.json()?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(Error::bad_request("a template needs a name"));
    }
    if !body.rule.is_object() {
        return Err(Error::bad_request("a template's `rule` is an object of rule fields"));
    }
    // A template's conditions are checked now, so the wizard never offers a
    // starting point that cannot be saved.
    for key in ["when", "unless"] {
        if let Some(condition) = body.rule.get(key).filter(|c| !c.is_null()) {
            condition::Condition::from_json(condition.clone())
                .and_then(|c| c.compile(key).map(|_| ()).map_err(|p| p.join("; ")))
                .map_err(Error::bad_request)?;
        }
    }

    let saved = resolve::<PolicyRepository>()?
        .save_template(
            name,
            body.description.filter(|d| !d.trim().is_empty()),
            body.category.as_deref().map(str::trim).filter(|c| !c.is_empty()).unwrap_or("custom"),
            body.rule,
        )
        .await?;
    Ok(Response::json(&saved.as_json()))
}

/// `DELETE /api/policy/templates/{template}`
pub async fn delete_template(request: Req) -> Result<Response> {
    let raw = route_param(&request, "template")?;
    let id: u64 = raw.parse().map_err(|_| Error::bad_request(format!("`{raw}` is not a template id")))?;
    if !resolve::<PolicyRepository>()?.delete_template(id).await? {
        return Err(Error::not_found(format!("there is no template {id}")));
    }
    Ok(Response::no_content())
}

// ---------------------------------------------------------------------------
// Shared.
// ---------------------------------------------------------------------------

/// Put `rule` into the document: in place of `original` when editing (and
/// renaming, if the names differ), at the end when adding.
fn put_rule(document: &mut PolicyDocument, original: Option<&str>, mut rule: Rule) -> std::result::Result<(), EditError> {
    rule.name = rule.name.trim().to_string();
    if rule.name.is_empty() {
        return Err(EditError::invalid("a rule needs a name"));
    }

    match original {
        Some(original) => {
            let index = document
                .rules
                .iter()
                .position(|r| r.name == original)
                .ok_or_else(|| EditError::not_found(format!("no rule is called `{original}`")))?;
            if rule.name != original && document.rule(&rule.name).is_some() {
                return Err(EditError::invalid(format!(
                    "a rule called `{}` already exists; two rules with one name would make the log \
                     ambiguous",
                    rule.name
                )));
            }
            document.rules[index] = rule;
        }
        None => {
            if document.rule(&rule.name).is_some() {
                return Err(EditError::invalid(format!(
                    "a rule called `{}` already exists; two rules with one name would make the log \
                     ambiguous",
                    rule.name
                )));
            }
            document.rules.push(rule);
        }
    }
    Ok(())
}

fn take_rule(document: &mut PolicyDocument, name: &str) -> std::result::Result<Rule, EditError> {
    let index = document
        .rules
        .iter()
        .position(|r| r.name == name)
        .ok_or_else(|| EditError::not_found(format!("no rule is called `{name}`")))?;
    Ok(document.rules.remove(index))
}

fn actor(request: &Request) -> String {
    request
        .header(ACTOR_HEADER)
        .map(|a| a.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(32).collect())
        .filter(|a: &String| !a.is_empty())
        .unwrap_or_else(|| "api".to_string())
}

fn meta(request: &Request, summary: &str) -> EditMeta {
    EditMeta::new(summary, actor(request))
        .based_on(request.header(REVISION_HEADER).and_then(|r| r.trim().parse().ok()))
}

/// The answer to every write: what is running now.
fn changed(result: std::result::Result<crate::app::services::Applied, EditError>) -> Result<Response> {
    let applied = result.map_err(edit_error)?;
    let store = resolve::<RuleStore>()?;
    Ok(Response::json(&serde_json::json!({
        "saved": true,
        "changed": applied.revision.is_some(),
        "revision": store.revision(),
        "change": applied.revision.as_ref().map(|r| r.summary.clone()),
        "rules": applied.rules.rules().len(),
        "profiles": applied.rules.profiles().len(),
        "loaded_at": applied.rules.loaded_at(),
    })))
}

/// Every problem at once, so a fix is one edit rather than five round trips.
fn edit_error(e: EditError) -> Error {
    let message = e.problems.join("\n");
    let error = match e.kind {
        EditErrorKind::Invalid => Error::bad_request(message),
        EditErrorKind::NotFound => Error::not_found(message),
        EditErrorKind::Conflict => Error::conflict(message),
        EditErrorKind::Storage => Error::internal(message),
    };
    error.with_details(serde_json::json!({ "problems": e.problems }))
}

fn route_param(request: &Request, name: &str) -> Result<String> {
    request
        .route_param(name)
        .map(str::to_string)
        .ok_or_else(|| Error::internal(format!("the route has no `{{{name}}}` parameter")))
}

fn revision_param(request: &Request) -> Result<u64> {
    let raw = route_param(request, "revision")?;
    raw.parse().map_err(|_| Error::bad_request(format!("`{raw}` is not a revision number")))
}
