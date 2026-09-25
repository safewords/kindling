//! Feature tests — the real kernel, the real routes, the real database.
//!
//! What is asserted here is the boot itself: the three-step conversation a
//! machine actually has with this server, and the things that go wrong in it.
//! The rule engine is covered in unit tests next to the code; these are about
//! the seams between it, the inventory and the wire.

// The boot lock is held across the boot's awaits deliberately: serialising the
// boot is the point of it. Safe because `#[tokio::test]` runs on a
// current-thread runtime, so the guard never crosses a thread.
#![allow(clippy::await_holding_lock)]

use std::sync::Arc;

use pxe::app::repositories::{BootEventRepository, HostRepository};
use pxe::app::services::BootService;
use pxe::pxe::mac::MacAddr;
use pxe::{boot, Mode};
use rainier_framework::prelude::*;
use rainier_framework::testing::TestApp;

static BOOTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A booted server, plus the handful of things a test reaches for.
struct Server {
    app: TestApp,
}

impl Server {
    async fn boot() -> Self {
        let app = {
            let _booting = BOOTING.lock().unwrap_or_else(|e| e.into_inner());
            boot(Mode::Testing).await.expect("the server should boot")
        };
        Self { app: TestApp::new(app).expect("a kernel") }
    }

    fn hosts(&self) -> Arc<HostRepository> {
        self.app.resolve::<HostRepository>().expect("the inventory")
    }

    fn events(&self) -> Arc<BootEventRepository> {
        self.app.resolve::<BootEventRepository>().expect("the boot log")
    }

    fn service(&self) -> Arc<BootService> {
        self.app.resolve::<BootService>().expect("the boot service")
    }

    /// What iPXE asks for once it has read the machine's own tables.
    fn boot_url(&self, mac: &str, extra: &str) -> String {
        format!("/boot.ipxe?mac={mac}&arch=x86_64&platform=efi{extra}")
    }
}

fn mac(text: &str) -> MacAddr {
    text.parse().expect("a MAC address")
}

#[tokio::test]
async fn the_health_endpoint_reports_the_policy_it_is_running() {
    let server = Server::boot().await;

    server
        .app
        .get("/api/health")
        .await
        .assert_ok()
        .assert_json_path("status", "ok")
        .assert_json_path("inventory.status", "ok");
}

#[tokio::test]
async fn a_machine_with_no_query_is_sent_back_to_ask_again_properly() {
    // The reflector. iPXE has just started and this server knows nothing about
    // the machine beyond an address it has not been told, so the first answer
    // is a script that asks.
    let server = Server::boot().await;

    let response = server.app.get("/boot.ipxe").await;
    response.assert_ok().assert_contains("#!ipxe").assert_contains("chain ");

    assert!(
        response.text().contains("${net0/mac:hexhyp}"),
        "the machine expands its own address: {}",
        response.text()
    );
    assert!(
        response.text().contains("${product:uristring}"),
        "and everything else it knows about itself: {}",
        response.text()
    );
}

#[tokio::test]
async fn a_machine_is_recorded_the_first_time_it_asks_and_not_declared_in_advance() {
    let server = Server::boot().await;
    let address = "18:66:da:11:22:33";

    assert!(server.hosts().by_mac(mac(address)).await.unwrap().is_none());

    server
        .app
        .get(&server.boot_url(address, "&product=OptiPlex%207090&manufacturer=Dell%20Inc."))
        .await
        .assert_ok();

    let host = server.hosts().by_mac(mac(address)).await.unwrap().expect("recorded");
    assert_eq!(host.vendor.as_deref(), Some("Dell"), "identified from the OUI, not the query");
    assert_eq!(host.product.as_deref(), Some("OptiPlex 7090"));
    assert_eq!(host.arch, "x64-uefi");
    assert_eq!(host.boot_count, 1);
}

#[tokio::test]
async fn a_machine_is_new_exactly_once() {
    // The property the `known = false` rule hangs on, and the one that is easy
    // to get wrong: the row is created before the decision is made, so reading
    // `known` off the row would make every machine known on its first boot.
    let server = Server::boot().await;
    let address = "52:54:00:aa:bb:cc";

    let first = server.app.get(&server.boot_url(address, "")).await;
    first.assert_ok();

    let second = server.app.get(&server.boot_url(address, "")).await;
    second.assert_ok();

    assert_ne!(
        first.text(),
        second.text(),
        "a machine that has booted here before is not a new machine"
    );

    let host = server.hosts().by_mac(mac(address)).await.unwrap().expect("recorded");
    assert_eq!(host.boot_count, 2);
}

#[tokio::test]
async fn every_stage_of_a_boot_leaves_a_line_in_the_log() {
    let server = Server::boot().await;
    let address = "18:66:da:44:55:66";

    server.app.get(&server.boot_url(address, "")).await.assert_ok();

    let events = server.events().for_mac(mac(address), 10).await.unwrap();
    assert!(!events.is_empty(), "serving a script is an event");

    let script = events.iter().find(|event| event.kind == "script").expect("a script event");
    assert!(script.profile.is_some(), "and it records what the machine was sent");
    assert!(script.rule.is_some(), "and which rule decided");
}

#[tokio::test]
async fn a_profile_can_be_fetched_by_name_and_an_unknown_one_is_a_404() {
    // This is where a menu selection lands, so the `.ipxe` suffix a menu adds
    // has to resolve to the same profile as the bare name.
    let server = Server::boot().await;

    server.app.get("/profiles/local").await.assert_ok().assert_contains("#!ipxe");
    server.app.get("/profiles/local.ipxe").await.assert_ok().assert_contains("#!ipxe");
    server.app.get("/profiles/no-such-profile.ipxe").await.assert_not_found();
}

#[tokio::test]
async fn writing_endpoints_are_closed_when_no_token_is_configured() {
    // Not open. This server decides what a fleet executes at power-on.
    let server = Server::boot().await;
    let address = "18:66:da:77:88:99";
    server.app.get(&server.boot_url(address, "")).await.assert_ok();

    server
        .app
        .post(&format!("/api/hosts/{address}/pin"), &serde_json::json!({ "profile": "local" }))
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);

    server
        .app
        .post_empty("/api/rules/reload")
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);

    // Reading is unaffected.
    server.app.get("/api/hosts").await.assert_ok();
    server.app.get(&format!("/api/hosts/{address}")).await.assert_ok();
}

#[tokio::test]
async fn a_pin_overrides_the_rules_and_a_one_shot_overrides_the_pin() {
    // Driven through the repository rather than the API, because the API is
    // closed without a token and what is under test is the precedence.
    let server = Server::boot().await;
    let address = "18:66:da:ab:cd:ef";
    server.app.get(&server.boot_url(address, "")).await.assert_ok();

    server.hosts().pin(mac(address), Some("memtest")).await.unwrap();
    let pinned = server.app.get(&server.boot_url(address, "")).await;
    pinned.assert_ok().assert_contains("memtest");

    server.hosts().set_once(mac(address), Some("shell")).await.unwrap();
    let once = server.app.get(&server.boot_url(address, "")).await;
    once.assert_ok().assert_contains("shell");

    // And the one-shot is spent, so the pin is back in force.
    let host = server.hosts().by_mac(mac(address)).await.unwrap().unwrap();
    assert_eq!(host.once_profile, None, "the one-shot was used up");
    assert_eq!(host.pinned_profile.as_deref(), Some("memtest"));

    server.app.get(&server.boot_url(address, "")).await.assert_ok().assert_contains("memtest");
}

#[tokio::test]
async fn a_tag_carves_a_machine_out_of_the_policy() {
    // What `pxe:tag --add=hold` is for: taking one machine out of a rule
    // without naming its address in the rule file.
    let server = Server::boot().await;
    let address = "18:66:da:de:ad:01";

    let before = server.app.get(&server.boot_url(address, "")).await;
    before.assert_ok();

    let mut host = server.hosts().by_mac(mac(address)).await.unwrap().unwrap();
    host.add_tag("hold");
    server.hosts().save(&host).await.unwrap();

    let after = server.app.get(&server.boot_url(address, "")).await;
    after.assert_ok();
    assert!(
        after.text().contains("sanboot") || after.text().contains("exit 0"),
        "a held machine boots its own disk: {}",
        after.text()
    );
}

#[tokio::test]
async fn the_rule_tester_explains_itself_and_changes_nothing() {
    let server = Server::boot().await;
    let address = "00:00:0c:11:22:33"; // Cisco: network gear

    let response = server
        .app
        .post("/api/rules/test", &serde_json::json!({ "mac": address, "arch": "bios" }))
        .await;

    response
        .assert_ok()
        .assert_json_path("decision.profile", "hands-off")
        .assert_json_path("facts.device_class", "network");

    let trace = response.json()["trace"].as_array().cloned().unwrap_or_default();
    assert!(!trace.is_empty(), "the trace is the point of the endpoint");

    // A dry run must not invent a machine.
    assert!(
        server.hosts().by_mac(mac(address)).await.unwrap().is_none(),
        "asking a question should not create a row"
    );
}

#[tokio::test]
async fn the_server_stays_quiet_for_a_machine_the_policy_says_to_ignore() {
    // `kind = "ignore"` is a real answer on a network that has another boot
    // server on it.
    let server = Server::boot().await;

    let facts = pxe::pxe::facts::ClientFacts::new(
        mac("00:00:0c:aa:bb:cc"),
        pxe::pxe::arch::ClientArch::BIOS,
        pxe::pxe::facts::Stage::Firmware,
    )
    .identified(server.service().ouis());

    let answer =
        pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*server.service(), facts).await;

    assert!(answer.is_none(), "a switch is offered nothing at all");
}

#[tokio::test]
async fn the_firmware_stage_chainloads_ipxe_and_the_ipxe_stage_does_not() {
    // The check that stops a machine chainloading iPXE from iPXE for ever.
    let server = Server::boot().await;
    let service = server.service();

    let firmware = pxe::pxe::facts::ClientFacts::new(
        mac("18:66:da:12:34:56"),
        pxe::pxe::arch::ClientArch::X64_UEFI,
        pxe::pxe::facts::Stage::Firmware,
    )
    .identified(service.ouis());

    let first =
        pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, firmware.clone())
            .await
            .expect("a physical machine is answered");
    assert!(first.file.ends_with(".efi"), "the loader, over TFTP: {}", first.file);

    let already_running = firmware.with_user_class(Some("iPXE".into()));
    let second = pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, already_running)
        .await
        .expect("answered");
    assert!(second.file.ends_with("/boot.ipxe"), "the script, over HTTP: {}", second.file);
}

#[tokio::test]
async fn every_admin_screen_is_served_the_same_shell() {
    // The interface is a single page application, so the server's whole job
    // here is to hand over a document with the bundle in it. Each path is
    // declared rather than caught by a fallback — on this server a mistyped
    // URL is as likely to be a machine asking for a boot file as a person, and
    // it should 404 rather than get HTML.
    let server = Server::boot().await;

    for path in ["/", "/devices", "/devices/18:66:da:11:22:33", "/policy", "/config", "/events"] {
        let response = server.app.get(path).await;
        response.assert_ok();
        assert!(
            response.text().contains("id=\"app\""),
            "`{path}` should serve the shell, got: {}",
            &response.text()[..response.text().len().min(200)]
        );
    }

    // And a path nobody declared is still a 404.
    server.app.get("/not-a-screen").await.assert_not_found();
}

#[tokio::test]
async fn the_shell_carries_the_built_bundle_and_the_server_facts() {
    // `@vite` resolves the content-hashed filenames out of the manifest, so a
    // shell that names none of them is one whose assets never built.
    let server = Server::boot().await;
    let response = server.app.get("/").await;
    response.assert_ok();

    let body = response.text();
    assert!(body.contains("/build/assets/"), "the bundle should be linked: {body}");

    // Handed over in the document so the first screen renders without a round
    // trip to find out what this server calls itself.
    assert!(body.contains("data-server="), "{body}");
    assert!(body.contains("data-base="), "{body}");
}

#[tokio::test]
async fn a_file_under_the_boot_root_is_served_and_a_path_outside_it_is_not() {
    let server = Server::boot().await;

    // The root ships a README, which is a real file to ask for.
    server.app.get("/boot/README.md").await.assert_ok();

    for attempt in ["/boot/../Cargo.toml", "/boot/../../etc/passwd", "/boot/.env"] {
        let response = server.app.get(attempt).await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "`{attempt}` resolved to something it should not have"
        );
    }
}

#[tokio::test]
async fn a_machine_chainloading_in_a_circle_is_given_the_script_it_was_trying_to_reach() {
    // The failure this server cannot detect any other way: iPXE that announces
    // itself neither by user class (option 77) nor by its own options
    // (option 175) — a relay stripped them, or the build never set them. Every
    // individual exchange looks perfectly correct, and the machine loops until
    // somebody walks over to it.
    let server = Server::boot().await;
    let service = server.service();
    let address = mac("18:66:da:10:0b:01");

    let lap = |xid: u32| {
        pxe::pxe::facts::ClientFacts::new(
            address,
            pxe::pxe::arch::ClientArch::X64_UEFI,
            pxe::pxe::facts::Stage::Firmware,
        )
        .with_vendor_class(Some("PXEClient:Arch:00007".into()))
        // Deliberately silent: this is the machine the detection misses.
        .with_transaction(Some(xid))
        .identified(service.ouis())
    };

    // The first laps are answered normally — being slow is not being lost.
    for xid in 0..3 {
        let answer = pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, lap(xid))
            .await
            .expect("answered");
        assert!(answer.file.ends_with(".efi"), "lap {xid}: {}", answer.file);
    }

    // By the fourth whole transaction it is plainly going in circles, and the
    // script is what it was trying to get to all along.
    let broken = pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, lap(3))
        .await
        .expect("answered");

    assert!(broken.file.ends_with("/boot.ipxe"), "{}", broken.file);
    assert_eq!(broken.reason, "the chainload was looping");

    // And it is in the boot log, not only in the process log.
    let events = server.events().for_mac(address, 10).await.unwrap();
    assert!(
        events.iter().any(|event| {
            event.detail.as_deref().is_some_and(|detail| detail.contains("looping"))
        }),
        "the recovery should be visible in `pxe:log`: {events:#?}"
    );
}

#[tokio::test]
async fn retransmitting_one_request_is_never_mistaken_for_a_loop() {
    // A lossy network makes firmware repeat its DISCOVER. Treating that as a
    // loop would break a boot that was only slow — so the transaction id, not
    // the request count, is what the breaker counts.
    let server = Server::boot().await;
    let service = server.service();
    let address = mac("18:66:da:10:0b:02");

    for _ in 0..10 {
        let facts = pxe::pxe::facts::ClientFacts::new(
            address,
            pxe::pxe::arch::ClientArch::X64_UEFI,
            pxe::pxe::facts::Stage::Firmware,
        )
        .with_vendor_class(Some("PXEClient:Arch:00007".into()))
        .with_transaction(Some(0xabcd_1234))
        .identified(service.ouis());

        let answer = pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, facts)
            .await
            .expect("answered");
        assert!(answer.file.ends_with(".efi"), "still the loader: {}", answer.file);
    }
}

#[tokio::test]
async fn ipxe_that_announces_itself_never_reaches_the_breaker_at_all() {
    // The normal path, asserted on so the backstop stays a backstop: option
    // 175 alone is enough, with no user class.
    let server = Server::boot().await;
    let service = server.service();

    let facts = pxe::pxe::facts::ClientFacts::new(
        mac("18:66:da:10:0b:03"),
        pxe::pxe::arch::ClientArch::X64_UEFI,
        pxe::pxe::facts::Stage::Firmware,
    )
    .with_vendor_class(Some("PXEClient:Arch:00007".into()))
    .with_ipxe_options(true)
    .with_transaction(Some(1))
    .identified(service.ouis());

    let answer = pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, facts)
        .await
        .expect("answered");

    assert!(answer.file.ends_with("/boot.ipxe"), "{}", answer.file);
    assert_eq!(answer.reason, "iPXE is asking, so it gets the script");
}

#[tokio::test]
async fn a_machine_is_still_new_when_the_stage_that_picks_an_image_is_reached() {
    // The bug this test exists for, found by running the thing: one boot is
    // several requests. The DHCP offer creates the inventory row, and the
    // script — the only request that picks an image — comes later. When
    // `known` meant "is there a row", it was already false by then, so
    // `known = false` fired at the stage where nothing is chosen and never at
    // the stage where something is. It could not do the one job it is for.
    let server = Server::boot().await;
    let service = server.service();
    let address = "18:66:da:5e:e9:01";

    // 1. The firmware asks. This creates the row.
    let firmware = pxe::pxe::facts::ClientFacts::new(
        mac(address),
        pxe::pxe::arch::ClientArch::X64_UEFI,
        pxe::pxe::facts::Stage::Firmware,
    )
    .with_vendor_class(Some("PXEClient:Arch:00007".into()))
    .with_transaction(Some(1))
    .identified(service.ouis());

    pxe::pxe::dhcp::proxy::BootPolicy::firmware_answer(&*service, firmware)
        .await
        .expect("offered a loader");

    assert!(
        server.hosts().by_mac(mac(address)).await.unwrap().is_some(),
        "the row exists from the first packet, which is the trap"
    );

    // 2. iPXE comes back for its script. This is where the image is chosen,
    //    and the machine has still never booted anything here.
    let script = server.app.get(&server.boot_url(address, "")).await;
    script.assert_ok();

    assert!(
        script.text().contains("Ubuntu 24.04"),
        "a machine that has never booted here should still be new when it \
         matters, and get the installer: {}",
        script.text()
    );

    // 3. And now it has booted, so the next one is not new.
    let second = server.app.get(&server.boot_url(address, "")).await;
    second.assert_ok();
    assert!(
        !second.text().contains("Ubuntu 24.04"),
        "the second boot is not a first boot: {}",
        second.text()
    );
}

/// A booted server whose writing endpoints have a token, for the editor tests.
///
/// The token has to be in the environment rather than set afterwards: the
/// guard resolves it once, when the middleware stack is built at boot. That it
/// *cannot* be changed later is the point of the design, not a limitation of
/// the test.
async fn editable() -> Server {
    let app = {
        let _booting = BOOTING.lock().unwrap_or_else(|e| e.into_inner());
        let mut env = pxe::environment(Mode::Testing);
        env.set("PXE_API_TOKEN", TEST_TOKEN);
        pxe::boot_with(Mode::Testing, env.isolated()).await.expect("the server should boot")
    };
    Server { app: TestApp::new(app).expect("a kernel") }
}

const TEST_TOKEN: &str = "test-token";

/// A write with the token, as the web UI sends it.
async fn write(
    server: &Server,
    method: rainier_framework::http::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> rainier_framework::testing::TestResponse {
    let mut request = server.app.request(method, path).header("x-pxe-token", TEST_TOKEN);
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.build().pipe(|request| server.app.send(request)).await
}

async fn policy(server: &Server) -> serde_json::Value {
    let response = server.app.get("/api/policy").await;
    response.assert_ok();
    response.json()
}

#[tokio::test]
async fn the_policy_comes_from_the_database_rule_by_rule() {
    let server = Server::boot().await;
    let body = policy(&server).await;

    assert_eq!(body["source"], "database");
    assert!(body["revision"].as_u64().is_some(), "the seeded starter is revision one: {body}");
    let rules = body["rules"].as_array().unwrap();
    assert!(!rules.is_empty(), "the starter policy was seeded");
    assert!(!body["profiles"].as_array().unwrap().is_empty());

    // Each rule carries its condition tree and the same thing in words.
    let network = rules.iter().find(|r| r["name"] == "never-touch-network-gear").unwrap();
    assert!(network["when"]["all"].is_array(), "{network}");
    assert!(network["described"]["when"].as_str().unwrap().contains("device_class"), "{network}");
}

#[tokio::test]
async fn the_schema_describes_every_fact_an_editor_can_offer() {
    let server = Server::boot().await;
    let schema = server.app.get("/api/policy/schema").await.json();

    let facts = schema["facts"].as_array().unwrap();
    let product = facts.iter().find(|f| f["name"] == "product").unwrap();
    assert_eq!(product["type"], "text");
    assert_eq!(product["known_at"], "ipxe");
    assert!(product["operators"].as_array().unwrap().iter().any(|o| o == "regex"));
    assert!(schema["operators"]["in_subnet"]["label"].is_string());
    assert!(schema["profile_kinds"].as_array().unwrap().len() >= 5);
}

#[tokio::test]
async fn a_rule_can_be_added_changed_toggled_and_removed_and_each_is_a_revision() {
    use rainier_framework::http::Method;
    let server = editable().await;
    let start = policy(&server).await["revision"].as_u64().unwrap();

    let rule = serde_json::json!({
        "name": "storage-nodes-get-arch",
        "priority": 300,
        "when": { "all": [
            { "fact": "product", "op": "glob", "value": ["PowerEdge R7*"] },
            { "not": { "fact": "tag", "op": "has_any", "value": ["hold"] } }
        ] },
        "profile": "arch-r630",
        "tag": ["storage"],
        "set": { "role": "storage" }
    });
    write(&server, Method::POST, "/api/policy/rules", Some(rule.clone())).await.assert_ok();

    let body = policy(&server).await;
    let stored = body["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "storage-nodes-get-arch")
        .unwrap()
        .clone();
    assert_eq!(stored["set"]["role"], "storage");
    assert_eq!(body["revision"].as_u64().unwrap(), start + 1);

    // Replacing it may rename it.
    let mut renamed = rule.clone();
    renamed["name"] = "r7xx-get-arch".into();
    write(&server, Method::PUT, "/api/policy/rules/storage-nodes-get-arch", Some(renamed))
        .await
        .assert_ok();
    // A patch changes only what it names; `null` clears.
    write(
        &server,
        Method::PATCH,
        "/api/policy/rules/r7xx-get-arch",
        Some(serde_json::json!({ "priority": 5, "set": null })),
    )
    .await
    .assert_ok();
    write(
        &server,
        Method::POST,
        "/api/policy/rules/r7xx-get-arch/enabled",
        Some(serde_json::json!({ "enabled": false })),
    )
    .await
    .assert_ok();

    let body = policy(&server).await;
    let stored =
        body["rules"].as_array().unwrap().iter().find(|r| r["name"] == "r7xx-get-arch").unwrap().clone();
    assert_eq!(stored["priority"], 5);
    assert_eq!(stored["enabled"], false);
    assert!(stored["set"].as_object().unwrap().is_empty(), "{stored}");
    assert_eq!(stored["profile"], "arch-r630", "the patch left the rest alone");

    write(&server, Method::DELETE, "/api/policy/rules/r7xx-get-arch", None).await.assert_ok();
    assert!(!policy(&server).await["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "r7xx-get-arch"));

    let history = server.app.get("/api/policy/revisions").await.json();
    let summaries: Vec<&str> =
        history["data"].as_array().unwrap().iter().map(|r| r["summary"].as_str().unwrap()).collect();
    assert!(summaries[0].contains("removed"), "{summaries:?}");
    assert_eq!(summaries.len() as u64, start + 5, "{summaries:?}");
}

#[tokio::test]
async fn a_change_that_would_not_load_is_refused_and_the_running_policy_is_untouched() {
    use rainier_framework::http::Method;
    let server = editable().await;
    let before = policy(&server).await;

    let refused = write(
        &server,
        Method::PUT,
        "/api/policy",
        Some(serde_json::json!({ "text": "[[rule]]\nname = \"x\"\nprofile = \"no-such-profile\"\n" })),
    )
    .await;
    refused.assert_status(StatusCode::BAD_REQUEST);
    assert!(refused.text().contains("no-such-profile"), "{}", refused.text());

    let refused = write(
        &server,
        Method::POST,
        "/api/policy/rules",
        Some(serde_json::json!({ "name": "bad", "profile": "local",
            "when": { "fact": "product", "op": "regex", "value": "(" } })),
    )
    .await;
    refused.assert_status(StatusCode::BAD_REQUEST);
    assert!(refused.text().contains("regular expression"), "{}", refused.text());

    // A profile still in use cannot be deleted.
    write(&server, Method::DELETE, "/api/policy/profiles/local", None)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let after = policy(&server).await;
    assert_eq!(before["document"], after["document"], "nothing was stored");
    assert_eq!(before["revision"], after["revision"]);
}

#[tokio::test]
async fn a_write_made_against_an_old_revision_is_refused() {
    // Two operators, one policy: the second save must not silently undo the
    // first.
    use rainier_framework::http::Method;
    let server = editable().await;
    let seen = policy(&server).await["revision"].as_u64().unwrap();

    write(
        &server,
        Method::POST,
        "/api/policy/rules/never-touch-network-gear/enabled",
        Some(serde_json::json!({ "enabled": false })),
    )
    .await
    .assert_ok();

    server
        .app
        .request(Method::POST, "/api/policy/rules/never-touch-network-gear/enabled")
        .header("x-pxe-token", TEST_TOKEN)
        .header("x-policy-revision", &seen.to_string())
        .json(&serde_json::json!({ "enabled": true }))
        .build()
        .pipe(|request| server.app.send(request))
        .await
        .assert_status(StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_earlier_revision_can_be_restored() {
    use rainier_framework::http::Method;
    let server = editable().await;
    let original = policy(&server).await;
    let first = original["revision"].as_u64().unwrap();

    write(&server, Method::DELETE, "/api/policy/rules/tag-by-subnet", None).await.assert_ok();
    assert_ne!(policy(&server).await["document"], original["document"]);

    write(&server, Method::POST, &format!("/api/policy/revisions/{first}/restore"), None)
        .await
        .assert_ok();
    let restored = policy(&server).await;
    assert_eq!(restored["document"], original["document"], "the policy is as it was");
    assert!(
        restored["revision"].as_u64().unwrap() > first,
        "and the rollback is itself a revision"
    );
}

#[tokio::test]
async fn renaming_a_profile_carries_every_reference_with_it() {
    use rainier_framework::http::Method;
    let server = editable().await;
    let body = policy(&server).await;
    let local = body["profiles"].as_array().unwrap().iter().find(|p| p["name"] == "local").unwrap()
        ["body"]
        .clone();

    let mut renamed = local.clone();
    renamed["rename_to"] = "disk".into();
    write(&server, Method::PUT, "/api/policy/profiles/local", Some(renamed)).await.assert_ok();

    let document = policy(&server).await["document"].clone();
    assert!(document["profiles"]["disk"].is_object());
    assert!(document["profiles"]["local"].is_null());
    let text = document.to_string();
    assert!(
        !text.contains("\"profile\":\"local\""),
        "no rule or menu still points at the old name: {text}"
    );
}

#[tokio::test]
async fn a_preview_says_which_known_machines_a_rule_would_change() {
    let server = editable().await;
    // A machine the inventory knows.
    let address = "18:66:da:77:88:99";
    server.app.get(&server.boot_url(address, "&product=OptiPlex%207090")).await.assert_ok();

    let preview = server
        .app
        .post(
            "/api/policy/preview",
            &serde_json::json!({ "rule": {
                "name": "optiplexes-get-memtest",
                "priority": 950,
                "when": { "fact": "product", "op": "glob", "value": "OptiPlex*" },
                "profile": "memtest"
            } }),
        )
        .await;
    preview.assert_ok();
    let body = preview.json();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["fires"], 1, "{body}");
    let row = &body["rows"][0];
    assert_eq!(row["mac"], address);
    assert_eq!(row["after"], "memtest");
    assert_eq!(row["changed"], true);

    // Nothing was stored by asking.
    assert!(!policy(&server).await["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "optiplexes-get-memtest"));
}

#[tokio::test]
async fn a_profile_can_be_rendered_before_it_is_saved() {
    let server = Server::boot().await;
    let body = server
        .app
        .post(
            "/api/policy/profiles/render",
            &serde_json::json!({
                "name": "debian",
                "profile": {
                    "kernel": "{{boot}}/debian/linux",
                    "initrd": ["{{boot}}/debian/initrd.gz"],
                    "cmdline": "auto=true hostname={{ var.role }}"
                }
            }),
        )
        .await
        .json();
    assert_eq!(body["ok"], true, "{body}");
    let script = body["script"].as_str().unwrap();
    assert!(script.starts_with("#!ipxe"), "{script}");
    assert!(script.contains("/boot/debian/linux"), "{script}");
}

#[tokio::test]
async fn the_wizard_can_save_and_forget_its_own_templates() {
    use rainier_framework::http::Method;
    let server = editable().await;

    let saved = write(
        &server,
        Method::POST,
        "/api/policy/templates",
        Some(serde_json::json!({
            "name": "Dell servers",
            "category": "who",
            "rule": { "when": { "fact": "vendor", "op": "glob", "value": "Dell*" } }
        })),
    )
    .await;
    saved.assert_ok();
    let id = saved.json()["id"].as_u64().unwrap();

    let list = server.app.get("/api/policy/templates").await.json();
    assert_eq!(list["data"][0]["name"], "Dell servers");

    write(
        &server,
        Method::POST,
        "/api/policy/templates",
        Some(serde_json::json!({
            "name": "broken", "rule": { "when": { "fact": "nope", "op": "is", "value": true } }
        })),
    )
    .await
    .assert_status(StatusCode::BAD_REQUEST);

    write(&server, Method::DELETE, &format!("/api/policy/templates/{id}"), None)
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_policy_exports_as_a_document_that_imports_back() {
    use rainier_framework::http::Method;
    let server = editable().await;
    let exported = server.app.get("/api/policy/export?format=json").await;
    exported.assert_ok();
    let document: serde_json::Value = serde_json::from_str(exported.text()).unwrap();

    let toml = server.app.get("/api/policy/export?format=toml").await;
    toml.assert_ok();

    // Importing what was exported changes nothing, so records no revision.
    let response =
        write(&server, Method::PUT, "/api/policy", Some(serde_json::json!({ "text": toml.text() })))
            .await;
    response.assert_ok();
    assert_eq!(response.json()["changed"], false, "{}", response.text());
    assert_eq!(policy(&server).await["document"], document);
}

#[tokio::test]
async fn validating_a_document_changes_nothing_and_needs_no_token() {
    // An editor that can only check by saving is one that teaches people to
    // save to find out.
    let server = Server::boot().await;

    server
        .app
        .post("/api/policy/validate", &serde_json::json!({
            "text": "[profiles.a]\nkind = \"local\"\n\n[[rule]]\nname = \"r\"\nprofile = \"a\"\n"
        }))
        .await
        .assert_ok()
        .assert_json_path("ok", true);

    let bad = server
        .app
        .post("/api/policy/validate", &serde_json::json!({ "text": "[[rule]]\nname = \"r\"\nprofile = \"gone\"\n" }))
        .await;
    bad.assert_ok().assert_json_path("ok", false);
    assert!(bad.text().contains("gone"), "{}", bad.text());
}

#[tokio::test]
async fn the_configuration_endpoint_documents_every_setting_and_hides_the_secrets() {
    let server = Server::boot().await;
    let response = server.app.get("/api/config").await;
    response.assert_ok();

    let body = response.json();
    let settings = body["settings"].as_array().expect("a catalogue");
    assert!(settings.len() > 15, "every setting this server reads");

    for setting in settings {
        assert!(
            setting["help"].as_str().unwrap_or("").len() > 40,
            "`{}` should explain itself",
            setting["key"]
        );
    }

    // A token in the file is never echoed back in full.
    let token = settings.iter().find(|s| s["key"] == "PXE_API_TOKEN").expect("the token setting");
    assert_eq!(token["kind"], "secret");
}

#[tokio::test]
async fn editing_the_configuration_needs_the_token_and_refuses_a_setting_nobody_reads() {
    let server = editable().await;

    // Without the token: closed, like every other write.
    server
        .app
        .request(rainier_framework::http::Method::PATCH, "/api/config")
        .json(&serde_json::json!({ "changes": { "SERVER_PORT": "9090" } }))
        .build()
        .pipe(|request| server.app.send(request))
        .await
        .assert_unauthorized();

    // With it, a name this server does not read is refused rather than
    // written into the file to sit there doing nothing.
    let refused = server
        .app
        .request(rainier_framework::http::Method::PATCH, "/api/config")
        .header("x-pxe-token", "test-token")
        .json(&serde_json::json!({ "changes": { "PXE_SREVER_IP": "10.0.0.2" } }))
        .build()
        .pipe(|request| server.app.send(request))
        .await;

    refused.assert_status(StatusCode::BAD_REQUEST);
    assert!(refused.text().contains("PXE_SREVER_IP"), "{}", refused.text());
}

/// A tiny helper so the request builder reads left to right.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for rainier_framework::http::Request {}

// --- the live feed -----------------------------------------------------------

/// Everything announced so far, parsed. Publishing is synchronous with the
/// write, so by the time a request has answered its messages are queued.
fn drain(watcher: &mut tokio::sync::broadcast::Receiver<Arc<str>>) -> Vec<serde_json::Value> {
    let mut heard = Vec::new();
    while let Ok(frame) = watcher.try_recv() {
        heard.push(serde_json::from_str(&frame).expect("every frame is JSON"));
    }
    heard
}

#[tokio::test]
async fn the_live_feed_is_served_on_the_http_port() {
    let server = Server::boot().await;
    let sockets = server
        .app
        .resolve::<rainier_framework::websocket::WebSocketRoutes>()
        .expect("socket routes are bound");
    assert!(sockets.match_path("/ws/live").is_some(), "{:?}", sockets.patterns());
}

#[tokio::test]
async fn a_boot_is_announced_as_it_is_recorded() {
    // The feed is fed from the repositories, so a machine arriving over HTTP
    // is announced without the boot controller knowing a browser exists.
    let server = Server::boot().await;
    let live = server.app.resolve::<pxe::app::services::LiveFeed>().expect("the feed");
    let mut watcher = live.subscribe();
    let address = "18:66:da:77:88:99";

    server.app.get(&server.boot_url(address, "&product=OptiPlex%207090")).await.assert_ok();

    let heard = drain(&mut watcher);
    let hosts: Vec<_> = heard.iter().filter(|m| m["type"] == "host").collect();
    let events: Vec<_> = heard.iter().filter(|m| m["type"] == "event").collect();

    assert!(!hosts.is_empty(), "the new machine was announced: {heard:?}");
    assert_eq!(hosts.last().unwrap()["data"]["mac"], address);
    assert_eq!(hosts.last().unwrap()["data"]["boot_count"], 1, "the last word is the counted boot");

    assert_eq!(events.len(), 1, "one script, one row: {heard:?}");
    assert_eq!(events[0]["data"]["kind"], "script");
    assert!(events[0]["data"]["id"].is_number(), "with the id the database gave it");
}

#[tokio::test]
async fn an_operator_s_change_is_announced_and_the_token_never_is() {
    let server = editable().await;
    let live = server.app.resolve::<pxe::app::services::LiveFeed>().expect("the feed");
    let address = "18:66:da:12:34:56";
    server.app.get(&server.boot_url(address, "")).await.assert_ok();

    let mut watcher = live.subscribe();

    server
        .app
        .request(rainier_framework::http::Method::POST, &format!("/api/hosts/{address}/pin"))
        .header("x-pxe-token", TEST_TOKEN)
        .json(&serde_json::json!({ "profile": "memtest" }))
        .build()
        .pipe(|request| server.app.send(request))
        .await
        .assert_ok();

    server
        .app
        .request(rainier_framework::http::Method::DELETE, &format!("/api/hosts/{address}"))
        .header("x-pxe-token", TEST_TOKEN)
        .build()
        .pipe(|request| server.app.send(request))
        .await;

    server
        .app
        .request(rainier_framework::http::Method::POST, "/api/rules/reload")
        .header("x-pxe-token", TEST_TOKEN)
        .build()
        .pipe(|request| server.app.send(request))
        .await
        .assert_ok();

    let heard = drain(&mut watcher);
    let kinds: Vec<&str> = heard.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["host", "host.forgotten", "policy"], "{heard:?}");
    assert_eq!(heard[0]["data"]["pinned_profile"], "memtest", "the row as it now is");
    assert_eq!(heard[2]["data"]["reloaded"], true);

    // Watching needs no token, so nothing announced may contain one.
    for message in &heard {
        assert!(!message.to_string().contains(TEST_TOKEN), "the token leaked: {message}");
    }
}
