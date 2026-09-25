//! What a person and a booting machine reach.

use rainier_framework::prelude::*;

use crate::app::http::controllers::{asset_controller, boot_controller, dashboard_controller};
use crate::app::http::kernel;

/// The admin interface's screens.
///
/// Declared rather than served by a catch-all. A catch-all answers a mistyped
/// URL with a page instead of a 404, and on this server a mistyped URL is as
/// likely to be a machine asking for a boot file as a person.
const SCREENS: &[(&str, &str)] = &[
    ("/", "dashboard"),
    ("/devices", "devices"),
    ("/devices/{mac}", "device"),
    ("/policy", "policy"),
    ("/config", "config"),
    ("/events", "events"),
    // Where the server-rendered pages used to be, kept so a bookmark still
    // lands somewhere.
    ("/hosts", "hosts"),
    ("/rules", "rules"),
];

pub fn routes(router: &mut Router) {
    // The boot endpoints first, and outside every group: they are the reason
    // this server exists, and nothing should come between a machine and its
    // script.
    router.group(GroupAttributes::new().middleware(kernel::boot()), |router| {
        router.get("/boot.ipxe", boot_controller::boot).name("boot");
        router.get("/profiles/{profile}", boot_controller::profile).name("boot.profile");
        router.get("/boot/{path*}", boot_controller::file).name("boot.file");
    });

    // The built frontend.
    router.get("/build/{path*}", asset_controller::build).name("assets");

    router.group(GroupAttributes::new().middleware(kernel::api()), |router| {
        for (path, name) in SCREENS {
            router.get(*path, dashboard_controller::shell).name(*name);
        }
    });
}
