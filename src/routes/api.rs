//! The management API.
//!
//! Split by what a call *does*, not by what it addresses: reading is open, and
//! everything that changes what a machine will boot — including the policy and
//! the configuration — is inside the guarded group. Reading that split off the
//! indentation is the point.

use rainier_framework::prelude::*;

use crate::app::http::controllers::{api_controller, config_controller, policy_controller};
use crate::app::http::kernel;

pub fn routes(router: &mut Router) {
    router.group(
        GroupAttributes::new().prefix("api").name("api.").middleware(kernel::api()),
        |router| {
            router.get("/health", api_controller::health).name("health");

            // --- the inventory ---
            router.get("/hosts", api_controller::hosts).name("hosts.index");
            router.get("/hosts/{host}", api_controller::show_host).name("hosts.show");
            router.get("/events", api_controller::events).name("events");
            router.get("/facets", api_controller::tags_in_use).name("facets");

            // --- the policy ---
            router.get("/rules", api_controller::rules).name("rules");
            router.get("/profiles", api_controller::profiles).name("profiles");
            router.get("/policy", policy_controller::show).name("policy.show");
            router.get("/policy/schema", policy_controller::schema).name("policy.schema");
            router.get("/policy/export", policy_controller::export).name("policy.export");
            router.get("/policy/revisions", policy_controller::revisions).name("policy.revisions");
            router
                .get("/policy/revisions/{revision}", policy_controller::show_revision)
                .name("policy.revisions.show");
            router.get("/policy/templates", policy_controller::templates).name("policy.templates");

            // --- the configuration ---
            router.get("/config", config_controller::show).name("config.show");

            // Checking a document changes nothing, so it is authorised as a
            // read. An editor that could only validate by saving would be an
            // editor that encourages saving to find out.
            router.post("/policy/validate", policy_controller::validate).name("policy.validate");
            // "What would this change do to my machines", asked before the
            // change is made. It reads the inventory and writes nothing.
            router.post("/policy/preview", policy_controller::preview).name("policy.preview");
            router
                .post("/policy/profiles/render", policy_controller::render_profile)
                .name("policy.profiles.render");
            router.post("/config/validate", config_controller::validate).name("config.validate");
            router.post("/rules/test", api_controller::test).name("rules.test");
        },
    );

    router.group(
        GroupAttributes::new().prefix("api").name("api.").middleware(kernel::guarded()),
        |router| {
            // --- machines ---
            router.post("/hosts/bulk", api_controller::bulk).name("hosts.bulk");
            router.post("/hosts/{host}/pin", api_controller::pin).name("hosts.pin");
            router.delete("/hosts/{host}/pin", api_controller::unpin).name("hosts.unpin");
            router.post("/hosts/{host}/once", api_controller::set_once).name("hosts.once");
            router
                .delete("/hosts/{host}/once", api_controller::clear_once)
                .name("hosts.once.clear");
            router.post("/hosts/{host}/tags", api_controller::tags).name("hosts.tags");
            router.delete("/hosts/{host}", api_controller::forget).name("hosts.forget");

            // --- the policy ---
            router.post("/rules/reload", api_controller::reload).name("rules.reload");
            router.put("/policy", policy_controller::replace).name("policy.replace");
            router.post("/policy/rules", policy_controller::add_rule).name("policy.rules.add");
            // Before the `{rule}` routes, so `reorder` is not taken for a name.
            router
                .post("/policy/rules/reorder", policy_controller::reorder_rules)
                .name("policy.rules.reorder");
            router
                .put("/policy/rules/{rule}", policy_controller::put_rule_endpoint)
                .name("policy.rules.put");
            router
                .patch("/policy/rules/{rule}", policy_controller::patch_rule)
                .name("policy.rules.update");
            router
                .delete("/policy/rules/{rule}", policy_controller::remove_rule)
                .name("policy.rules.remove");
            router
                .post("/policy/rules/{rule}/enabled", policy_controller::set_rule_enabled)
                .name("policy.rules.enabled");
            router
                .put("/policy/profiles/{profile}", policy_controller::put_profile)
                .name("policy.profiles.put");
            router
                .delete("/policy/profiles/{profile}", policy_controller::remove_profile)
                .name("policy.profiles.remove");
            router
                .put("/policy/settings", policy_controller::put_settings)
                .name("policy.settings");
            router
                .put("/policy/bootloaders/{arch}", policy_controller::put_bootloader)
                .name("policy.bootloaders");
            router
                .post("/policy/revisions/{revision}/restore", policy_controller::restore_revision)
                .name("policy.revisions.restore");
            router
                .post("/policy/templates", policy_controller::save_template)
                .name("policy.templates.save");
            router
                .delete("/policy/templates/{template}", policy_controller::delete_template)
                .name("policy.templates.delete");

            // --- the configuration ---
            router.put("/config", config_controller::replace).name("config.replace");
            router.patch("/config", config_controller::patch).name("config.patch");
        },
    );
}
