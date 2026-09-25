//! What a booting machine talks to.
//!
//! Three endpoints, and the order they are used in is the boot:
//!
//! 1. `/boot.ipxe` with no query — the **reflector**. iPXE has just started
//!    and this server knows nothing about the machine beyond its address, so
//!    it hands back a two-line script that asks iPXE to come back with
//!    everything its firmware and SMBIOS tables know.
//! 2. `/boot.ipxe?mac=…&product=…` — the real decision, made against facts no
//!    DHCP packet could have carried.
//! 3. `/profiles/<name>.ipxe` — one profile by name, which is where a menu
//!    selection lands.
//!
//! Plus `/boot/…`, which serves the TFTP root over HTTP: the same bytes, for
//! the machines that can fetch them faster.

use std::sync::Arc;

use rainier_framework::prelude::*;
use rainier_framework::public::PublicFiles;

use crate::app::models::{BootEvent, EventKind};
use crate::app::services::BootService;
use crate::pxe::arch::{parse_arch, ClientArch};
use crate::pxe::facts::{ClientFacts, Stage};
use crate::pxe::mac::MacAddr;

pub(crate) fn resolve<T: Send + Sync + 'static>() -> Result<Arc<T>> {
    rainier_framework::container::facade_application().resolve::<T>()
}

/// `GET /boot.ipxe`
/// What `loader=` said before the project was renamed.
const LEGACY_OWN_LOADER: &str = "safewords";

pub async fn boot(request: Req) -> Result<Response> {
    let service = resolve::<BootService>()?;

    // No `mac` means iPXE has not told us who it is yet. Rather than guess
    // from the source address — which is right until the moment a machine is
    // behind NAT or has just changed address — send it back with the question
    // asked properly.
    let Some(mac) = query_mac(&request) else {
        return Ok(ipxe(reflector(&service.settings().http_base)));
    };

    let facts = facts_from_query(&request, mac);
    let script = service.script(facts).await?;
    Ok(ipxe(script))
}

/// `GET /profiles/{profile}` — with or without the `.ipxe` suffix a menu adds.
pub async fn profile(request: Req) -> Result<Response> {
    let service = resolve::<BootService>()?;

    let name = request
        .route_param("profile")
        .ok_or_else(|| Error::internal("the route has no `{profile}` parameter"))?
        .trim_end_matches(".ipxe")
        .to_string();

    if service.rules().current().profile(&name).is_none() {
        return Err(Error::not_found(format!("no profile is called `{name}`")));
    }

    let mac = query_mac(&request).unwrap_or(MacAddr::ZERO);
    let facts = facts_from_query(&request, mac);

    let script = service.named_profile(&name, facts).await?;
    Ok(ipxe(script))
}

/// `GET /boot/{path*}` — the TFTP root, over HTTP.
pub async fn file(request: Req) -> Result<Response> {
    let service = resolve::<BootService>()?;
    let files = resolve::<PublicFiles>()?;

    // `PublicFiles` resolves against the request path, and this route is
    // mounted a directory deep, so the prefix comes off first.
    let relative = request.path().trim_start_matches('/').trim_start_matches("boot").to_string();

    let served = files.serve(&rebased(&request, &relative)).await;


    // Only the loaders are logged, not every chunk of every image. A machine
    // fetching its boot loader is the event that says an offer was acted on;
    // the thirty requests for kernel and initrd that follow say nothing that
    // the first one did not.
    if is_boot_loader(&relative) {
        let peer = request.ip().map(|ip| ip.to_string());
        let mac = query_mac(&request).map(|mac| mac.to_string());
        service
            .note(BootEvent::plain(
                mac.or_else(|| peer.clone().map(|ip| format!("ip:{ip}")))
                    .unwrap_or_else(|| "unknown".into()),
                if served.is_successful() { EventKind::Http } else { EventKind::Refused },
                format!("{relative} ({})", served.status().as_u16()),
                peer,
            ))
            .await;
    }

    Ok(served)
}

/// The two-line script that turns one address into a full set of facts.
///
/// Everything after the first `&` is an iPXE variable, expanded on the machine
/// — which is the only place any of it is known. `:uristring` is iPXE's own
/// URI encoding, and it is on every free-text field because a product name
/// with a space in it would otherwise truncate the query string.
pub fn reflector(base: &str) -> String {
    let base = base.trim_end_matches('/');
    format!(
        "#!ipxe\n\
         # Asking again, with everything this machine knows about itself.\n\
         chain {base}/boot.ipxe?\
mac=${{net0/mac:hexhyp}}\
&uuid=${{uuid}}\
&arch=${{buildarch}}\
&platform=${{platform}}\
&manufacturer=${{manufacturer:uristring}}\
&product=${{product:uristring}}\
&serial=${{serial:uristring}}\
&asset=${{asset:uristring}}\
&hostname=${{hostname:uristring}}\n"
    )
}

fn query_mac(request: &Request) -> Option<MacAddr> {
    request.input("mac").and_then(|raw| raw.parse().ok())
}

/// Build the facts from what iPXE reflected back.
fn facts_from_query(request: &Request, mac: MacAddr) -> ClientFacts {
    let ouis = resolve::<crate::pxe::oui::OuiDatabase>().ok();

    // `buildarch` is iPXE's word for the binary it is: `i386`, `x86_64`,
    // `arm64`. It is not the firmware architecture number, so it is mapped
    // rather than parsed, and the platform says which firmware is under it.
    let platform = request.input("platform").map(|p| p.to_lowercase());
    let arch = match request.input("arch").map(|a| a.to_lowercase()).as_deref() {
        Some("x86_64") | Some("x64") => match platform.as_deref() {
            Some("pcbios") => ClientArch::BIOS,
            _ => ClientArch::X64_UEFI,
        },
        Some("i386") => match platform.as_deref() {
            Some("pcbios") => ClientArch::BIOS,
            _ => ClientArch::X86_UEFI,
        },
        Some("arm64") | Some("aarch64") => ClientArch::ARM64_UEFI,
        // Anything else may be an architecture label or a number, which is
        // what `pxe:test --arch` passes.
        other => other.and_then(parse_arch).unwrap_or(ClientArch::X64_UEFI),
    };

    let facts = ClientFacts::new(mac, arch, Stage::Ipxe)
        .with_uuid(non_empty(request.input("uuid")))
        .with_hostname(non_empty(request.input("hostname")))
        .with_platform(non_empty(request.input("platform")))
        .with_client_ip(request.ip())
        // Our own build says so in the query (`loader=kindling`, from
        // `ipxe/embed.ipxe`), since an HTTP request carries no user class.
        // Builds from before the rename say `safewords`.
        .with_user_class(Some(
            match request.input("loader").as_deref() {
                Some("kindling") | Some(LEGACY_OWN_LOADER) => crate::pxe::facts::OWN_IPXE_USER_CLASS,
                _ => "iPXE",
            }
            .to_string(),
        ))
        .with_smbios(
            non_empty(request.input("manufacturer")),
            non_empty(request.input("product")),
            non_empty(request.input("serial")),
            non_empty(request.input("asset")),
        );

    match ouis {
        Some(ouis) => facts.identified(&ouis),
        None => facts,
    }
}

/// iPXE substitutes an empty string for a variable it does not have, and an
/// empty string is not the same as "this machine has no product name" — it is
/// the same as not knowing. Both become `None` here so a rule matching on
/// `product` does not fire for a machine that reported nothing.
fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn is_boot_loader(path: &str) -> bool {
    let path = path.to_lowercase();
    [".efi", ".kpxe", ".kkpxe", ".pxe", ".lkrn", ".0", ".ipxe"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

/// The same request, asked about a path relative to the boot directory.
///
/// `PublicFiles` resolves against the request's own path and this route is
/// mounted a directory deep, so the prefix has to come off somewhere. Building
/// a request rather than mutating one keeps the conditional-request header,
/// which is what makes a repeated fetch of a 300MB image a `304`.
fn rebased(request: &Request, relative: &str) -> Request {
    let mut builder = Request::builder()
        .method(request.method().clone())
        .uri(format!("/{}", relative.trim_start_matches('/')));

    if let Some(tag) = request.header("if-none-match") {
        builder = builder.header("if-none-match", tag);
    }
    builder.build()
}

fn ipxe(script: String) -> Response {
    // `text/plain` deliberately. iPXE does not care what the type is, and a
    // human debugging a boot wants the script to open in a browser rather than
    // download.
    Response::new(StatusCode::OK)
        .with_header("content-type", "text/plain; charset=utf-8")
        .with_header("cache-control", "no-store")
        .with_body(script.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reflector_asks_ipxe_for_what_only_ipxe_knows() {
        let script = reflector("http://10.0.0.2:8080/");
        assert!(script.starts_with("#!ipxe\n"));
        assert!(script.contains("chain http://10.0.0.2:8080/boot.ipxe?mac=${net0/mac:hexhyp}"), "{script}");
        assert!(script.contains("product=${product:uristring}"), "{script}");
        assert!(!script.contains("//boot.ipxe"), "the trailing slash was handled: {script}");
    }

    #[test]
    fn free_text_fields_are_uri_encoded_so_a_space_does_not_truncate_the_query() {
        // `OptiPlex 7090` unencoded ends the query string at the space.
        let script = reflector("http://x");
        for field in ["manufacturer", "product", "serial", "asset", "hostname"] {
            assert!(
                script.contains(&format!("{field}=${{{field}:uristring}}")),
                "`{field}` is free text and must be encoded: {script}"
            );
        }
    }

    #[test]
    fn a_loader_is_logged_and_an_image_chunk_is_not() {
        // Otherwise one boot writes thirty rows that say nothing the first one
        // did not.
        assert!(is_boot_loader("ipxe.efi"));
        assert!(is_boot_loader("ipxe/undionly.kpxe"));
        assert!(is_boot_loader("pxelinux.0"));
        assert!(is_boot_loader("menu.ipxe"));

        assert!(!is_boot_loader("images/ubuntu/vmlinuz"));
        assert!(!is_boot_loader("images/ubuntu/initrd"));
        assert!(!is_boot_loader("images/win11/sources/boot.wim"));
    }

    #[test]
    fn an_empty_ipxe_variable_is_not_a_fact() {
        // iPXE substitutes "" for a variable it does not have, and a rule
        // matching `product = ["*"]` must not fire on that.
        assert_eq!(non_empty(Some("".into())), None);
        assert_eq!(non_empty(Some("   ".into())), None);
        assert_eq!(non_empty(Some(" OptiPlex 7090 ".into())), Some("OptiPlex 7090".into()));
        assert_eq!(non_empty(None), None);
    }
}
