//! The admin interface's shell.
//!
//! One template for every screen. The pages are Vue, the router is in the
//! browser, and the server's only job here is to hand over a document with the
//! bundle in it — so exactly one place knows what the interface is built from.
//!
//! Every screen this serves is a read of the API, and the API is the contract.
//! Nothing the interface can do is unavailable to `curl` or to the console
//! commands, deliberately: a boot server that can only be driven by a browser
//! is one nobody can script.

use rainier_framework::config::Config;
use rainier_framework::prelude::*;
use rainier_framework::view::View as ViewTemplate;

use crate::app::http::controllers::boot_controller::resolve;
use crate::config::keys::{APP_NAME, PXE_HTTP_BASE, PXE_SERVER_IP};

/// Every admin route renders this.
pub async fn shell() -> Result<Response> {
    let settings = resolve::<Config>()?;
    let name = settings.get_or(APP_NAME, "kindling".to_string());

    // Handed over in the document so the first screen needs no round trip to
    // say what this server is. `server` in particular is worth showing
    // everywhere: it is the address every machine is told to come back to,
    // and the one setting whose being wrong explains everything else.
    let view = ViewTemplate::new("app")
        .add("title", &name)?
        .add("name", &name)?
        .add("server", settings.get_or(PXE_SERVER_IP, String::new()))?
        .add("base", settings.get_or(PXE_HTTP_BASE, String::new()))?
        .add("version", build_info!().version)?;

    Ok(Html(View::instance().render_view(&view)?).into_response())
}
